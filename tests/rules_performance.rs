//! Phase 3 performance rule golden tests (DESIGN §11.1, §7.3).
//!
//! Each rule fixture directory holds `invalid.py` (true positives),
//! `valid.py` (near misses that must stay silent), `branch.py` (an
//! Unknown / Maybe branch case that must lower confidence or stay silent),
//! and `suppressed.py` (an inline suppression that must win). The golden
//! expectations pin the exact rule, file, line, column, severity, and
//! confidence of every diagnostic the whole directory produces; `valid.py`
//! and `suppressed.py` are silent by virtue of not appearing.

use std::path::{Path, PathBuf};

use manim_lint::application::check;
use manim_lint::cli::CheckArgs;
use manim_lint::diagnostic::{Confidence, Severity};
use manim_lint::reporting::OutputFormat;
use manim_lint::rules::registry;

fn fixture_dir(rule: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/rules")
        .join(rule)
}

/// Copies every fixture file of one rule into a fresh temp project.
fn copy_fixture(rule: &str) -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    for entry in std::fs::read_dir(fixture_dir(rule)).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            std::fs::copy(&path, project.path().join(path.file_name().unwrap())).unwrap();
        }
    }
    project
}

fn args_for(root: &Path) -> CheckArgs {
    CheckArgs {
        paths: vec![root.to_path_buf()],
        format: Some(OutputFormat::Json),
        ..CheckArgs::default()
    }
}

/// Runs `check` over a rule fixture directory and renders each diagnostic
/// as `path:line:column rule severity confidence`, in pipeline order.
fn observed(root: &Path) -> Vec<String> {
    observed_with(&args_for(root))
}

/// Like [`observed`], but with explicit `check` arguments (select-gating
/// tests override `--select`).
fn observed_with(args: &CheckArgs) -> Vec<String> {
    let report = check(args).unwrap();
    for diagnostic in &report.diagnostics {
        assert!(
            !diagnostic.evidence.is_empty(),
            "{}: evidence must be machine-readable and non-empty",
            diagnostic.rule_id
        );
        assert!(
            diagnostic.explanation.is_some(),
            "{}: explanation must state the Manim semantics",
            diagnostic.rule_id
        );
        assert!(
            !diagnostic.applicable_profiles.is_empty(),
            "{}: applicable profiles must be listed",
            diagnostic.rule_id
        );
    }
    report
        .diagnostics
        .iter()
        .map(|diagnostic| {
            format!(
                "{path}:{line}:{column} {rule} {severity} {confidence}",
                path = diagnostic.path,
                line = diagnostic.primary_span.start.line,
                column = diagnostic.primary_span.start.column,
                rule = diagnostic.rule_id,
                severity = diagnostic.severity,
                confidence = diagnostic.confidence,
            )
        })
        .collect()
}

fn assert_golden(rule: &str, expected: &[&str]) {
    let project = copy_fixture(rule);
    let rows = observed(project.path());
    assert_eq!(rows, expected, "golden diagnostics for {rule}");
    assert!(
        !rows.iter().any(|row| row.contains("suppressed.py")),
        "{rule}: inline suppression must win"
    );
}

#[test]
fn mlp201_hot_expensive_construction_golden() {
    // `invalid.py` animates an unrelated `anchor`, so both callbacks
    // provably execute during the play (liveness gate). `valid.py` (cold
    // constructions, DecimalNumber + set_value, cold helper, and the
    // `Suspended` scene whose updater is suspended by its own `FadeIn`
    // and sees only a static wait — provably never per frame) and
    // `branch.py` (a callback variable whose identity cannot be resolved
    // statically) stay silent.
    assert_golden(
        "MLP201",
        &[
            "invalid.py:8:47 MLP201 warning high",
            "invalid.py:9:39 MLP201 warning high",
        ],
    );
}

#[test]
fn mlp204_hot_scene_graph_growth_golden() {
    // `invalid.py` animates an unrelated `anchor`: the scene updater is
    // never suspended and the mobject updater's host is un-targeted, so
    // both callbacks provably execute (liveness gate). `valid.py`
    // (re-adding an existing object: reorder-only) and `branch.py` (a
    // helper call whose freshness is unproven) stay silent.
    assert_golden(
        "MLP204",
        &[
            "invalid.py:8:37 MLP204 warning high",
            "invalid.py:9:38 MLP204 warning high",
        ],
    );
}

#[test]
fn mlp205_static_unfrozen_wait_golden() {
    // `valid.py` (time-based updater makes the wait truly dynamic; no
    // frozen_frame kwarg; sub-threshold duration) and `branch.py` (an
    // updater registered on only some paths joins to Maybe) stay silent.
    assert_golden("MLP205", &["invalid.py:9:9 MLP205 warning high"]);
}

#[test]
fn mlp206_sub_frame_duration_golden() {
    // `valid.py` (durations of at least one frame, unknown durations) and
    // `branch.py` (a module constant is not a literal at the call site)
    // stay silent.
    assert_golden(
        "MLP206",
        &[
            "invalid.py:7:9 MLP206 warning certain",
            "invalid.py:8:9 MLP206 warning certain",
        ],
    );
}

#[test]
fn mlp220_unbounded_traced_path_golden() {
    // `valid.py` (a dissipating_time bound; a short span) and `branch.py`
    // (branch-dependent scene membership joins to Maybe) stay silent.
    assert_golden("MLP220", &["invalid.py:7:17 MLP220 warning high"]);
}

#[test]
fn mlp226_frame_varying_resource_key_golden() {
    // The tracker-driven play (`tracker.animate.set_value`) leaves both
    // `always_redraw` hosts un-targeted, so their callbacks provably
    // execute (liveness gate). The frame-varying f-string key carries
    // only MLP226 (it supersedes MLP201 on that span); the static-key
    // sibling construction carries MLP201; `branch.py` (string
    // concatenation is not a proven frame-varying key) falls back to
    // MLP201.
    assert_golden(
        "MLP226",
        &[
            "branch.py:7:39 MLP201 warning high",
            "invalid.py:7:39 MLP226 warning high",
            "invalid.py:8:40 MLP201 warning high",
        ],
    );
}

/// `--select MLP226` still computes the lifecycle and cost facts the rule
/// declares (capability gating, DESIGN §6.3): the narrow select reports
/// exactly the MLP226 rows of the golden set.
#[test]
fn mlp226_still_fires_under_a_narrow_select() {
    let project = copy_fixture("MLP226");
    let mut args = args_for(project.path());
    args.select = vec!["MLP226".to_owned()];
    assert_eq!(
        observed_with(&args),
        vec!["invalid.py:7:39 MLP226 warning high"]
    );
}

/// `--select MLP201` must not resurrect the generic diagnostic at the
/// span MLP226 owns: the enabled-rule closure keeps the superseding rule
/// running even though its own diagnostics are filtered out afterwards,
/// so the select-gated pipeline matches the pre-gating output exactly.
#[test]
fn selecting_a_superseded_rule_does_not_resurrect_it() {
    let project = copy_fixture("MLP226");
    let mut args = args_for(project.path());
    args.select = vec!["MLP201".to_owned()];
    assert_eq!(
        observed_with(&args),
        vec![
            "branch.py:7:39 MLP201 warning high",
            "invalid.py:8:40 MLP201 warning high",
        ]
    );
}

/// The review's canonical false positive (DESIGN §3.2/§3.3): `FadeIn(label)`
/// suspends the animated mobject's updaters for the whole play
/// (`animation.py` `begin` → `suspend_updating`), and the un-`dt`'d
/// `always_redraw` factory leaves the default `wait(3)` a static frame
/// (`scene.py` `should_update_mobjects`) — the callback runs about once,
/// never once per frame. MLP226/MLP201 must stay silent instead of
/// fabricating a "~120 distinct keys" claim; the scene's *real* defect —
/// the tracker-driven updater is frozen while the wait renders — is the
/// correctness diagnostic on the static wait.
#[test]
fn mlp226_suspended_always_redraw_is_never_a_performance_claim() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("scene.py"),
        "\
from manim import *


class Demo(Scene):
    def construct(self):
        tracker = ValueTracker(0)
        label = always_redraw(lambda: MathTex(f\"x = {tracker.get_value():.2f}\"))
        self.add(label)
        self.play(FadeIn(label), run_time=2)
        self.wait(3)
",
    )
    .unwrap();
    let rows = observed(project.path());
    assert!(
        !rows
            .iter()
            .any(|row| row.contains("MLP226") || row.contains("MLP201")),
        "a provably-suspended always_redraw must not carry a per-frame \
         performance claim: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.contains("MLC112")),
        "the frozen tracker-driven updater is the scene's real defect: {rows:?}"
    );
}

#[test]
fn mlp226_quantifies_keys_only_from_literal_durations() {
    let project = copy_fixture("MLP226");
    let report = check(&args_for(project.path())).unwrap();
    let diagnostic = report
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.rule_id == "MLP226")
        .expect("MLP226 fires on invalid.py");
    // 8 s at the default 60 fps profile: about 480 distinct keys, derived
    // from the literal duration of the one play that provably executes
    // the callback (the tracker-driven play; liveness-gated).
    assert!(
        diagnostic.message.contains("~480"),
        "distinct-key estimate must come from the literal 8 s play: {}",
        diagnostic.message
    );
    let keys = diagnostic
        .evidence
        .get("distinct_resource_keys")
        .expect("evidence carries the key-count bounds");
    assert_eq!(keys["lower"], serde_json::json!(480));
    assert_eq!(keys["upper"], serde_json::json!(480));
}

/// `MLP220` anchors on the `TracedPath` constructor (the path's internal
/// point history grows per frame); `MLP204` anchors on a per-frame
/// `Scene.add` of a fresh mobject (the user's scene graph grows per
/// frame). The declared `MLP220 supersedes MLP204` edge only collapses
/// SAME-span duplicates (DESIGN §7.3) and these anchors never coincide,
/// so one scenario containing both patterns reports both defects, each at
/// its own span — this is not a double report of one defect.
#[test]
fn mlp220_and_mlp204_are_distinct_defects_that_co_report() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("scene.py"),
        "\
from manim import *


class Demo(Scene):
    def construct(self):
        dot = Dot()
        trace = TracedPath(dot.get_center)
        marker = Square()
        self.add(dot, trace, marker)
        marker.add_updater(lambda m: self.add(Square()))
        self.play(dot.animate.shift(RIGHT), run_time=4)
        self.play(dot.animate.shift(UP), run_time=3)
",
    )
    .unwrap();
    assert_eq!(
        observed(project.path()),
        [
            "scene.py:7:17 MLP220 warning high",
            "scene.py:10:38 MLP204 warning high",
        ]
    );
}

#[test]
fn mlp220_evidence_quantifies_span_and_points() {
    let project = copy_fixture("MLP220");
    let report = check(&args_for(project.path())).unwrap();
    let diagnostic = report
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.rule_id == "MLP220")
        .expect("MLP220 fires on invalid.py");
    let span = diagnostic
        .evidence
        .get("alive_span_seconds")
        .expect("evidence carries the alive span");
    assert_eq!(span["lower"], serde_json::json!(7));
    assert_eq!(span["upper"], serde_json::json!(7));
    let points = diagnostic
        .evidence
        .get("estimated_points")
        .expect("evidence carries the point estimate");
    assert_eq!(points["lower"], serde_json::json!(420));
    assert!(
        diagnostic.evidence.contains_key("growth"),
        "growth orders (points O(F), raster O(F^2)) must be in evidence"
    );
}

#[test]
fn mlp202_hot_family_copy_golden() {
    // `invalid.py`'s play targets the unrelated `backup`, so the group's
    // updaters provably execute (liveness gate). `valid.py` (a per-frame
    // `become(copy)` of a single Square: family 1, gate shut; a cold
    // `group.copy()` in construct) and `branch.py` (a group grown in a
    // loop: the family widens to an open-above interval that never
    // confirms the gate) stay silent. `invalid.py`'s discarded
    // `m.copy()` updater body is additionally a provable no-op, so
    // `MLP215` fires on the registration alongside the two `MLP202`
    // per-call rows — distinct defects at distinct spans.
    assert_golden(
        "MLP202",
        &[
            "invalid.py:8:9 MLP215 warning high",
            "invalid.py:8:37 MLP202 warning high",
            "invalid.py:9:37 MLP202 warning high",
        ],
    );
}

#[test]
fn mlp203_hot_family_walk_golden() {
    // `invalid.py` animates an unrelated `anchor` so the group's updater
    // provably executes (liveness gate); its walk feeds `move_to`, so no
    // other rule sees a no-op. `valid.py` (a single `next_to` on a small
    // Dot and a `get_family` on a lone Square — the DESIGN §7.3
    // near-misses) and `branch.py` (loop-grown family: open-above
    // interval) stay silent.
    assert_golden("MLP203", &["invalid.py:8:47 MLP203 info high"]);
}

#[test]
fn mlp207_transform_begin_gate_golden() {
    // `valid.py` (Square → Circle: family 2, curve delta 4 — both gates
    // shut) and `branch.py` (loop-grown source family: open-above
    // interval never confirms) stay silent. The fixture pyproject lowers
    // min-confidence so the medium-confidence diagnostic surfaces.
    assert_golden("MLP207", &["invalid.py:9:19 MLP207 info medium"]);
}

#[test]
fn mlp208_text_family_transform_golden() {
    // `valid.py` (Square → Circle transform: no Text kind; a
    // `Text("Short title")` transform whose literal content stays below
    // the 32-character gate — a small title transform is idiomatic, not
    // the catalog's "large Text / MathTex family") and `branch.py` (an
    // unresolvable helper value: kind unknown, never guessed) stay
    // silent. `invalid.py` proves 45 content characters from the
    // literal.
    assert_golden("MLP208", &["invalid.py:9:19 MLP208 info high"]);
}

#[test]
fn mlp211_hot_large_allocation_golden() {
    // `invalid.py` animates an unrelated `anchor` so the updaters
    // provably execute (liveness gate); each allocation feeds `move_to`
    // so no other rule sees a no-op. `valid.py` (a cold 800 KB buffer in
    // construct; a per-frame 24-byte coordinate vector below the gate)
    // and `branch.py` (a non-literal length: bytes Unknown, never
    // guessed) stay silent. The second row is a literal tuple shape
    // (`np.zeros((400, 400))`).
    assert_golden(
        "MLP211",
        &[
            "invalid.py:9:44 MLP211 info medium",
            "invalid.py:10:44 MLP211 info medium",
        ],
    );
}

#[test]
fn mlp212_long_translucent_full_screen_animation_golden() {
    // Only an exact FullScreenRectangle, stable literal opacity in (0, 1),
    // a certain direct target animation, and a proven duration >= 5 s
    // pass. Short, opaque, opacity-changing, smaller, unknown, and
    // branch-only near misses stay silent.
    assert_golden(
        "MLP212",
        &[
            "alias.py:8:19 MLP212 info medium",
            "invalid.py:8:19 MLP212 info medium",
            "invalid.py:14:19 MLP212 info medium",
        ],
    );
}

#[test]
fn mlp213_calibrated_cairo_surface_mismatch_golden() {
    // Literal 32x32 / 40x40 Surfaces cross the versioned 1,024-face
    // calibration gate and are certain play targets. Smaller, unknown,
    // unused, and branch-only Surfaces stay silent.
    assert_golden(
        "MLP213",
        &[
            "alias.py:6:19 MLP213 info medium",
            "invalid.py:6:19 MLP213 info medium",
        ],
    );
}

#[test]
fn mlp223_transparent_positive_width_stroke_golden() {
    // Exact zero stroke opacity, exact positive width, a non-empty path,
    // certain per-frame capture, and absence of every future opacity/style
    // write are all required. The valid fixture exercises each refusal.
    assert_golden(
        "MLP223",
        &[
            "alias.py:7:9 MLP223 info high",
            "invalid.py:7:9 MLP223 info high",
            "invalid.py:13:9 MLP223 info high",
        ],
    );
}

#[test]
fn cairo_only_cost_rules_stay_silent_under_opengl() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("pyproject.toml"),
        "[tool.manim-lint]\n\
         select = [\"MLP213\", \"MLP223\"]\n\
         min-confidence = \"medium\"\n\
         default-profile = \"opengl\"\n\
         \n\
         [[tool.manim-lint.profile]]\n\
         name = \"opengl\"\n\
         renderer = \"opengl\"\n",
    )
    .unwrap();
    std::fs::write(
        project.path().join("scene.py"),
        "\
from manim import *


class Demo(ThreeDScene):
    def construct(self):
        surface = Surface(lambda u, v: (u, v, 0), resolution=(32, 32))
        circle = Circle(stroke_opacity=0, stroke_width=8)
        self.play(surface.animate.shift(RIGHT), circle.animate.shift(UP))
",
    )
    .unwrap();
    assert_eq!(observed(project.path()), Vec::<String>::new());
}

#[test]
fn mlp216_stable_geometry_redraw_golden() {
    // `valid.py` (`Circle(num_components=14)`: a banned kwarg voids the
    // curated topology proof; a `VGroup(...)` factory constructs no
    // provable leaf) and `branch.py` (a named-function factory is outside
    // the lambda-scoped proof) stay silent.
    assert_golden(
        "MLP216",
        &[
            "invalid.py:7:16 MLP216 info medium",
            "invalid.py:8:15 MLP216 info medium",
        ],
    );
}

#[test]
fn mlp224_point_from_proportion_golden() {
    // The 32-circle path proves exactly 1024 points, so `MLP224` claims
    // the `point_from_proportion` query; the generic `MLP203` must stay
    // out even though the family gate would also pass (specificity,
    // DESIGN §7.3). `invalid.py` animates an unrelated `anchor` so the
    // updater provably executes (liveness gate). `valid.py` (a lone
    // Circle: 32 points) and `branch.py` (loop-grown path: open-above
    // interval) stay silent.
    assert_golden("MLP224", &["invalid.py:8:46 MLP224 info high"]);
}

#[test]
fn mlp202_evidence_carries_gate_and_real_bounds() {
    let project = copy_fixture("MLP202");
    let report = check(&args_for(project.path())).unwrap();
    let diagnostic = report
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.rule_id == "MLP202")
        .expect("MLP202 fires on invalid.py");
    // The 40-square group plus itself: 41 family members, exactly.
    let family = diagnostic
        .evidence
        .get("family_size")
        .expect("evidence carries the family bounds");
    assert_eq!(family["lower"], serde_json::json!(41));
    assert_eq!(family["upper"], serde_json::json!(41));
    let gates = diagnostic
        .evidence
        .get("gates")
        .and_then(|gates| gates.as_array())
        .expect("evidence carries the emission gates");
    assert!(
        gates
            .iter()
            .any(|gate| gate["threshold"] == "large-family-gate" && gate["confirmed"] == true),
        "the large-family gate must be cited as confirmed: {gates:?}"
    );
}

#[test]
fn mlp208_evidence_omits_unprovable_numbers() {
    let project = copy_fixture("MLP208");
    let report = check(&args_for(project.path())).unwrap();
    let diagnostic = report
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.rule_id == "MLP208")
        .expect("MLP208 fires on invalid.py");
    // Text / TeX cardinalities are content-dependent: the kind facts are
    // present, the unprovable curve insertion is absent — never a
    // fabricated number (DESIGN §15).
    assert!(
        diagnostic.evidence.contains_key("source_kind"),
        "kind evidence fires the specialization"
    );
    assert!(
        !diagnostic.evidence.contains_key("curve_insertion"),
        "no fabricated curve numbers for content-dependent kinds"
    );
    assert!(
        !diagnostic.evidence.contains_key("family_size"),
        "the structural family count of a Text source is not meaningful \
         glyph evidence and must be omitted"
    );
    // The size gate that admitted the diagnostic is cited: the literal
    // constructor content proves a lower bound of rendered characters.
    let content_gate = diagnostic
        .evidence
        .get("content_gate")
        .expect("the content gate evidence is present");
    assert_eq!(
        content_gate
            .get("proven")
            .and_then(serde_json::Value::as_u64),
        Some(45),
        "the literal proves 45 content characters"
    );
    assert_eq!(
        content_gate
            .get("confirmed")
            .and_then(serde_json::Value::as_bool),
        Some(true),
        "the content gate is confirmed"
    );
}

#[test]
fn performance_rule_metadata_matches_the_design_catalog() {
    let expected: [(&str, Severity, Confidence, &[&str]); 9] = [
        ("MLP201", Severity::Warning, Confidence::High, &[]),
        ("MLP204", Severity::Warning, Confidence::High, &[]),
        ("MLP205", Severity::Warning, Confidence::High, &[]),
        ("MLP206", Severity::Warning, Confidence::Certain, &[]),
        ("MLP212", Severity::Info, Confidence::Medium, &[]),
        ("MLP213", Severity::Info, Confidence::Medium, &[]),
        (
            "MLP220",
            Severity::Warning,
            Confidence::High,
            &["MLP204", "MLP211"],
        ),
        ("MLP223", Severity::Info, Confidence::High, &[]),
        ("MLP226", Severity::Warning, Confidence::High, &["MLP201"]),
    ];
    for (rule, severity, confidence, supersedes) in expected {
        let metadata = registry::metadata_for(rule)
            .unwrap_or_else(|| panic!("{rule} must be registered as implemented"));
        assert!(metadata.default_enabled, "{rule} defaults to enabled");
        assert_eq!(metadata.default_severity, severity, "{rule} severity");
        assert_eq!(metadata.minimum_confidence, confidence, "{rule} confidence");
        assert_eq!(metadata.implementation_phase, 3, "{rule} phase");
        assert_eq!(metadata.supersedes, supersedes, "{rule} supersedes");
        assert!(
            metadata
                .required_capabilities
                .iter()
                .any(|capability| *capability == "cost-facts" || *capability == "lifecycle"),
            "{rule} must declare the fact layer it relies on"
        );
    }
}

#[test]
fn cardinality_tranche_metadata_matches_the_design_catalog() {
    let expected: [(&str, Severity, Confidence, &[&str]); 7] = [
        ("MLP202", Severity::Warning, Confidence::High, &[]),
        ("MLP203", Severity::Info, Confidence::High, &[]),
        ("MLP207", Severity::Info, Confidence::Medium, &[]),
        ("MLP208", Severity::Info, Confidence::High, &["MLP207"]),
        ("MLP211", Severity::Info, Confidence::Medium, &[]),
        ("MLP216", Severity::Info, Confidence::Medium, &[]),
        ("MLP224", Severity::Info, Confidence::High, &["MLP203"]),
    ];
    for (rule, severity, confidence, supersedes) in expected {
        let metadata = registry::metadata_for(rule)
            .unwrap_or_else(|| panic!("{rule} must be registered as implemented"));
        assert!(metadata.default_enabled, "{rule} defaults to enabled");
        assert_eq!(metadata.default_severity, severity, "{rule} severity");
        assert_eq!(metadata.minimum_confidence, confidence, "{rule} confidence");
        assert_eq!(metadata.implementation_phase, 3, "{rule} phase");
        assert_eq!(metadata.supersedes, supersedes, "{rule} supersedes");
        assert!(
            metadata.required_capabilities.contains(&"cost-facts"),
            "{rule} must declare the cost-fact layer it relies on"
        );
    }
}
