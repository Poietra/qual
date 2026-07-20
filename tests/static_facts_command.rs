//! Acceptance tests for the `static-facts` producer and CLI contract.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use manim_lint::application::{StaticFactsReport, static_facts};
use manim_lint::cli::StaticFactsArgs;
use serde_json::Value;

const SCHEMA: &str = include_str!("../schemas/static-facts-v0.json");

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/lifecycle")
        .join(name)
}

fn facts(path: &Path) -> StaticFactsReport {
    static_facts(&StaticFactsArgs {
        paths: vec![path.to_path_buf()],
        ..StaticFactsArgs::default()
    })
    .expect("StaticFacts analysis succeeds")
}

fn project(source: &[u8], pyproject: Option<&str>) -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("temporary project");
    std::fs::write(directory.path().join("scene.py"), source).expect("write scene");
    if let Some(pyproject) = pyproject {
        std::fs::write(directory.path().join("pyproject.toml"), pyproject)
            .expect("write configuration");
    }
    directory
}

fn schema_errors(instance: &Value) -> Vec<String> {
    let schema: Value = serde_json::from_str(SCHEMA).expect("schema is JSON");
    let validator = jsonschema::validator_for(&schema).expect("compile schema");
    validator
        .iter_errors(instance)
        .map(|error| error.to_string())
        .collect()
}

fn scene_id<'a>(document: &'a Value, suffix: &str) -> &'a str {
    document["scenes"]
        .as_array()
        .expect("scene array")
        .iter()
        .find(|scene| {
            scene["qualified_name"]
                .as_str()
                .is_some_and(|name| name.ends_with(suffix))
        })
        .and_then(|scene| scene["id"].as_str())
        .expect("scene exists")
}

#[test]
fn producer_output_validates_against_the_published_schema() {
    let lifecycle_fixtures = fixture("helper_plays.py");
    let report = facts(lifecycle_fixtures.parent().unwrap());
    let errors = schema_errors(&report.document);
    assert!(errors.is_empty(), "schema errors:\n{}", errors.join("\n"));
    assert_eq!(
        serde_json::from_str::<Value>(&report.output).expect("output is JSON"),
        report.document
    );
    assert!(report.output.ends_with('\n'));
}

#[test]
fn repeated_helper_calls_have_distinct_bounded_contexts_and_ids() {
    let source = br"from manim import Scene, Square

class Demo(Scene):
    def make(self):
        square = Square()
        self.add(square)

    def construct(self):
        self.make()
        self.make()
";
    let directory = project(source, None);
    let report = facts(directory.path());
    let demo = scene_id(&report.document, ".Demo");
    let objects: Vec<&Value> = report.document["objects"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|object| object["scene_id"] == demo)
        .collect();

    assert_eq!(objects.len(), 2);
    assert_eq!(
        objects[0]["allocation_anchor"],
        objects[1]["allocation_anchor"]
    );
    assert_ne!(objects[0]["call_context"], objects[1]["call_context"]);
    assert_ne!(objects[0]["id"], objects[1]["id"]);
}

#[test]
fn loop_allocations_are_never_projected_as_singletons() {
    let report = facts(&fixture("loops.py"));
    let objects = report.document["objects"].as_array().unwrap();
    assert!(!objects.is_empty());
    assert!(
        objects.iter().all(|object| {
            matches!(object["cardinality"].as_str(), Some("many" | "maybe-many"))
        })
    );
}

#[test]
fn transform_and_replacement_transform_remain_distinct() {
    let report = facts(&fixture("replacement.py"));
    let kinds: Vec<&str> = report.document["animations"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|animation| {
            animation["kind_candidates"]["values"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
        })
        .collect();
    assert!(kinds.contains(&"manim.animation.transform.Transform"));
    assert!(kinds.contains(&"manim.animation.transform.ReplacementTransform"));

    let replacement = report.document["animations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|animation| {
            animation["kind_candidates"]["values"]
                .as_array()
                .is_some_and(|values| {
                    values
                        .iter()
                        .any(|value| value == "manim.animation.transform.ReplacementTransform")
                })
        })
        .unwrap();
    assert_eq!(
        replacement["effects"]["replacement_targets"]["status"],
        "known"
    );
    assert_eq!(
        replacement["effects"]["replacement_targets"]["object_ids"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // The v0 producer cannot yet reconstruct before/after display order.
    // That field is reasoned Unknown, so coverage must not claim complete.
    assert_eq!(report.document["coverage"]["completeness"], "partial");
    assert!(
        report.document["coverage"]["frontiers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|frontier| frontier["kind"] == "unsupported-semantics")
    );
}

#[test]
fn dynamic_calls_create_reasoned_unknown_frontiers() {
    let source = br"from manim import Scene

class Demo(Scene):
    def construct(self):
        unknown_factory()()
";
    let directory = project(source, None);
    let report = facts(directory.path());
    let frontiers = report.document["coverage"]["frontiers"].as_array().unwrap();
    assert!(frontiers.iter().any(|frontier| {
        frontier["reasons"].as_array().is_some_and(|reasons| {
            reasons
                .iter()
                .any(|reason| reason["kind"] == "dynamic-call-target")
        })
    }));
    assert_eq!(report.document["coverage"]["completeness"], "partial");
}

#[test]
fn renderer_risk_projection_reports_blockers_without_permissions() {
    let source = br#"import random
from pathlib import Path
from manim import Square, ThreeDScene, always_redraw

class Demo(ThreeDScene):
    def construct(self):
        label = always_redraw(lambda: Square())
        label.add_updater(lambda mob, dt: mob.rotate(dt))
        random.random()
        Path("state.txt").write_text("state")
        self.move_camera(phi=1)
        self.wait(stop_condition=lambda: False)
"#;
    let directory = project(source, None);
    let report = facts(directory.path());
    let risks = report.document["renderer_risks"].as_array().unwrap();
    let kinds: BTreeSet<&str> = risks
        .iter()
        .filter_map(|risk| risk["kind"].as_str())
        .collect();
    for expected in [
        "active-updater",
        "always-redraw",
        "camera-mutation",
        "external-state-or-io",
        "randomness",
        "stop-condition",
    ] {
        assert!(
            kinds.contains(expected),
            "missing risk {expected}: {kinds:?}"
        );
    }
    assert!(risks.iter().all(|risk| {
        risk.get("safe_to_skip_render").is_none() && risk.get("safe_to_fork").is_none()
    }));
}

#[test]
fn shift_jis_japanese_source_uses_utf8_byte_and_unicode_anchors() {
    let decoded = "# coding: shift_jis\nfrom manim import Scene, Square\n\nclass 日本(Scene):\n    def construct(self):\n        四角 = Square()\n        self.add(四角)\n";
    let (encoded, _, had_errors) = encoding_rs::SHIFT_JIS.encode(decoded);
    assert!(!had_errors);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("シーン.py");
    std::fs::write(&path, encoded.as_ref()).unwrap();

    let report = facts(directory.path());
    let file = &report.document["files"][0];
    assert_eq!(file["path"], "シーン.py");
    assert_eq!(file["encoding"], "shift_jis");
    let object = &report.document["objects"][0];
    let anchor = &object["allocation_anchor"];
    assert_eq!(anchor["encoding"], "shift_jis");
    assert_eq!(anchor["unicode_span"]["start"]["line"], 6);
    assert_eq!(anchor["unicode_span"]["start"]["column"], 14);
    let start = usize::try_from(anchor["utf8_byte_range"]["start"].as_u64().unwrap()).unwrap();
    let end = usize::try_from(anchor["utf8_byte_range"]["end"].as_u64().unwrap()).unwrap();
    assert_eq!(&decoded[start..end], "Square()");
    assert!(schema_errors(&report.document).is_empty());
}

#[test]
fn output_is_byte_identical_across_runs_and_worker_counts() {
    let args = StaticFactsArgs {
        paths: vec![fixture("helper_plays.py")],
        ..StaticFactsArgs::default()
    };
    let run_with = |workers| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .build()
            .unwrap()
            .install(|| static_facts(&args).unwrap().output)
    };
    assert_eq!(run_with(1), run_with(4));
    assert_eq!(run_with(4), run_with(4));
}

#[test]
fn rule_selection_does_not_change_static_facts() {
    let source = b"from manim import Scene, Square\n\nclass Demo(Scene):\n    def construct(self):\n        self.add(Square())\n";
    let first = project(source, Some("[tool.manim-lint]\nselect = [\"MLC\"]\n"));
    let second = project(
        source,
        Some("[tool.manim-lint]\nselect = [\"MLP\"]\nignore = [\"MLP225\"]\n"),
    );
    assert_eq!(facts(first.path()).output, facts(second.path()).output);
}

#[test]
fn analysis_never_executes_analyzed_python() {
    let source = br#"from pathlib import Path
Path("EXECUTED").write_text("bad")
raise RuntimeError("must not run")

from manim import Scene
class Demo(Scene):
    def construct(self):
        pass
"#;
    let directory = project(source, None);
    let report = facts(directory.path());
    assert_eq!(
        report.document["files"][0]["analysis"]["status"],
        "analyzed"
    );
    assert!(!directory.path().join("EXECUTED").exists());
}
