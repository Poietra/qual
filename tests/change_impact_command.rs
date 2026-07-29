//! Acceptance tests for the `ChangeImpact` v0 producer and public schema.

use std::collections::BTreeSet;
use std::path::Path;

use qual::application::{ChangeImpactReport, change_impact, static_facts};
use qual::cli::{ChangeImpactArgs, StaticFactsArgs};
use serde_json::Value;

const SCHEMA: &str = include_str!("../schemas/change-impact-v0.json");

fn write_project(root: &Path, helper_body: &str) {
    std::fs::write(
        root.join("base.py"),
        format!(
            "from manim import FadeIn, Scene\n\nclass Base(Scene):\n    def show(self, mob):\n        {helper_body}\n"
        ),
    )
    .unwrap();
    for (path, class, shape) in [("a.py", "A", "Square"), ("b.py", "B", "Circle")] {
        std::fs::write(
            root.join(path),
            format!(
                "from manim import {shape}\nfrom base import Base\n\nclass {class}(Base):\n    def construct(self):\n        mob = {shape}()\n        self.show(mob)\n"
            ),
        )
        .unwrap();
    }
}

fn args(before: &Path, after: &Path) -> ChangeImpactArgs {
    ChangeImpactArgs {
        before: before.to_path_buf(),
        after: after.to_path_buf(),
        profile: None,
        renderer: None,
        fps: None,
        resolution: None,
    }
}

fn schema_errors(instance: &Value) -> Vec<String> {
    let schema: Value = serde_json::from_str(SCHEMA).expect("schema is JSON");
    let validator = jsonschema::validator_for(&schema).expect("compile schema");
    validator
        .iter_errors(instance)
        .map(|error| error.to_string())
        .collect()
}

fn impact(before: &Path, after: &Path) -> ChangeImpactReport {
    change_impact(&args(before, after)).expect("ChangeImpact succeeds")
}

#[test]
fn producer_validates_and_shared_helper_reaches_both_scenes_plays_and_objects() {
    let before = tempfile::tempdir().unwrap();
    let after = tempfile::tempdir().unwrap();
    write_project(before.path(), "self.play(FadeIn(mob))");
    write_project(after.path(), "self.play(FadeIn(mob), run_time=2)");

    let report = impact(before.path(), after.path());
    let errors = schema_errors(&report.document);
    assert!(errors.is_empty(), "schema errors:\n{}", errors.join("\n"));
    assert_eq!(
        serde_json::from_str::<Value>(&report.output).unwrap(),
        report.document
    );
    assert!(report.output.ends_with('\n'));

    let scenes: BTreeSet<&str> = report.document["affected_scenes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|scene| scene["qualified_name"].as_str())
        .collect();
    assert!(scenes.contains("a.A"));
    assert!(scenes.contains("b.B"));
    assert!(
        !report.document["affected_plays"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !report.document["affected_objects"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn every_affected_id_is_owned_by_the_named_static_facts_snapshot() {
    let before = tempfile::tempdir().unwrap();
    let after = tempfile::tempdir().unwrap();
    write_project(before.path(), "self.play(FadeIn(mob))");
    write_project(after.path(), "self.play(FadeIn(mob), run_time=2)");
    let report = impact(before.path(), after.path());
    let base_facts = static_facts(&StaticFactsArgs {
        paths: vec![before.path().to_path_buf()],
        ..StaticFactsArgs::default()
    })
    .unwrap();
    let target_facts = static_facts(&StaticFactsArgs {
        paths: vec![after.path().to_path_buf()],
        ..StaticFactsArgs::default()
    })
    .unwrap();

    for (field, facts_field) in [
        ("affected_scenes", "scenes"),
        ("affected_plays", "plays"),
        ("affected_objects", "objects"),
    ] {
        for candidate in report.document[field].as_array().unwrap() {
            let facts = if candidate["snapshot"] == "base" {
                &base_facts.document
            } else {
                &target_facts.document
            };
            assert!(
                facts[facts_field]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entity| entity["id"] == candidate["id"])
            );
        }
    }
}

#[test]
fn schema_rejects_empty_unknown_reasons_and_extra_fields() {
    let before = tempfile::tempdir().unwrap();
    let after = tempfile::tempdir().unwrap();
    std::fs::write(before.path().join("scene.py"), "def run(fn):\n    fn()\n").unwrap();
    std::fs::write(
        after.path().join("scene.py"),
        "def run(fn):\n    return fn()\n",
    )
    .unwrap();
    let report = impact(before.path(), after.path());
    assert!(schema_errors(&report.document).is_empty());
    assert_eq!(report.document["completeness"], "candidates");

    let mut empty_reasons = report.document.clone();
    empty_reasons["unknown_frontiers"][0]["reasons"] = Value::Array(Vec::new());
    assert!(!schema_errors(&empty_reasons).is_empty());

    let mut extra = report.document;
    extra["safe_to_skip_render"] = Value::Bool(true);
    assert!(!schema_errors(&extra).is_empty());
}

#[test]
fn reached_starred_play_is_incomplete_and_widens_object_candidates() {
    let before = tempfile::tempdir().unwrap();
    let after = tempfile::tempdir().unwrap();
    let scene = |shape: &str| {
        format!(
            "from manim import Circle, FadeIn, Scene, Square\n\nclass Demo(Scene):\n    def construct(self):\n        square = Square()\n        circle = Circle()\n        animations = [FadeIn({shape})]\n        self.play(*animations)\n"
        )
    };
    std::fs::write(before.path().join("scene.py"), scene("square")).unwrap();
    std::fs::write(after.path().join("scene.py"), scene("circle")).unwrap();
    let report = impact(before.path(), after.path());
    assert_eq!(report.document["completeness"], "candidates");
    assert!(
        report.document["unknown_frontiers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|frontier| {
                frontier["dependent"]["kind"] == "play"
                    && frontier["reasons"].as_array().is_some_and(|reasons| {
                        reasons
                            .iter()
                            .any(|reason| reason["kind"] == "star-arguments")
                    })
            })
    );
    for snapshot in ["base", "target"] {
        assert_eq!(
            report.document["affected_objects"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|object| object["snapshot"] == snapshot)
                .count(),
            2
        );
    }
    assert!(schema_errors(&report.document).is_empty());
}

#[test]
fn output_is_byte_identical_across_worker_counts() {
    let before = tempfile::tempdir().unwrap();
    let after = tempfile::tempdir().unwrap();
    write_project(before.path(), "self.play(FadeIn(mob))");
    write_project(after.path(), "self.play(FadeIn(mob), run_time=2)");
    let args = args(before.path(), after.path());
    let run_with = |workers| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .build()
            .unwrap()
            .install(|| change_impact(&args).unwrap().output)
    };
    assert_eq!(run_with(1), run_with(4));
    assert_eq!(run_with(4), run_with(4));
}

#[test]
fn shift_jis_anchors_and_static_execution_boundary_are_preserved() {
    let before = tempfile::tempdir().unwrap();
    let after = tempfile::tempdir().unwrap();
    for (directory, value) in [(before.path(), 1), (after.path(), 2)] {
        let decoded = format!(
            "# coding: shift_jis\nfrom pathlib import Path\nPath('EXECUTED').write_text('bad')\nfrom manim import Scene\n\nclass 日本(Scene):\n    def construct(self):\n        値 = {value}\n"
        );
        let (encoded, _, errors) = encoding_rs::SHIFT_JIS.encode(&decoded);
        assert!(!errors);
        std::fs::write(directory.join("シーン.py"), encoded.as_ref()).unwrap();
    }
    let report = impact(before.path(), after.path());
    let definition = report.document["changed_definitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|definition| definition["qualified_name"] == "シーン.日本.construct")
        .unwrap();
    assert_eq!(definition["base_anchor"]["encoding"], "shift_jis");
    assert_eq!(definition["target_anchor"]["encoding"], "shift_jis");
    assert!(!before.path().join("EXECUTED").exists());
    assert!(!after.path().join("EXECUTED").exists());
    assert!(schema_errors(&report.document).is_empty());
}
