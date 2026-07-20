//! Acceptance tests for bounded source patch generation and rematching.

use std::path::Path;

use manim_lint::application::{source_bridge, static_facts};
use manim_lint::cli::{SourceBridgeArgs, StaticFactsArgs};
use serde_json::{Value, json};

const REQUEST_SCHEMA: &str = include_str!("../schemas/source-bridge-request-v0.json");
const OUTPUT_SCHEMA: &str = include_str!("../schemas/source-bridge-v0.json");

fn schema_errors(schema: &str, instance: &Value) -> Vec<String> {
    let schema: Value = serde_json::from_str(schema).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    validator
        .iter_errors(instance)
        .map(|error| error.to_string())
        .collect()
}

fn project(source: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("scene.py"), source).unwrap();
    directory
}

fn facts(project: &Path) -> manim_lint::application::StaticFactsReport {
    static_facts(&StaticFactsArgs {
        paths: vec![project.to_path_buf()],
        ..StaticFactsArgs::default()
    })
    .unwrap()
}

fn target_object(document: &Value, binding: &str) -> Value {
    document["objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|object| {
            object["binding_candidates"]["values"]
                .as_array()
                .is_some_and(|values| values.iter().any(|value| value == binding))
        })
        .unwrap()
        .clone()
}

fn run(project: &Path, request: &Value) -> manim_lint::application::SourceBridgeReport {
    let request_file = project.join("request.json");
    std::fs::write(&request_file, serde_json::to_vec_pretty(&request).unwrap()).unwrap();
    source_bridge(&SourceBridgeArgs {
        path: project.to_path_buf(),
        request: request_file,
        profile: None,
        renderer: None,
        fps: None,
        resolution: None,
    })
    .unwrap()
}

fn object_request(document: &Value, object: &Value, operation: &Value) -> Value {
    json!({
        "schema_version": 0,
        "snapshot_id": document["snapshot"]["id"],
        "target_id": object["id"],
        "operation": operation,
    })
}

const SIMPLE: &str = "from manim import RIGHT, Scene, Square\n\nclass Demo(Scene):\n    def construct(self):\n        square = Square()\n        self.add(square)\n";

#[test]
fn inserts_shift_chain_reanalyzes_and_rematches_without_writing() {
    let directory = project(SIMPLE);
    let facts = facts(directory.path());
    let object = target_object(&facts.document, "square");
    let request = object_request(
        &facts.document,
        &object,
        &json!({ "kind": "insert-shift-chain", "argument": "RIGHT * 2" }),
    );
    assert!(schema_errors(REQUEST_SCHEMA, &request).is_empty());
    let report = run(directory.path(), &request);
    let errors = schema_errors(OUTPUT_SCHEMA, &report.document);
    assert!(errors.is_empty(), "schema errors:\n{}", errors.join("\n"));
    assert_eq!(report.document["status"], "unique", "{}", report.output);
    let candidate = &report.document["candidates"][0];
    assert_eq!(candidate["validation"]["status"], "accepted");
    assert_eq!(candidate["validation"]["parse"], "valid");
    assert_eq!(candidate["validation"]["rematch"]["status"], "match");
    assert_eq!(candidate["validation"]["coverage"]["status"], "preserved");
    assert_eq!(candidate["edits"][0]["original_text"], "");
    assert_eq!(candidate["edits"][0]["replacement"], ".shift(RIGHT * 2)");
    assert_eq!(
        std::fs::read_to_string(directory.path().join("scene.py")).unwrap(),
        SIMPLE
    );
}

#[test]
fn modifies_existing_shift_and_lists_multiple_matches_without_choosing() {
    let source = "from manim import LEFT, RIGHT, Scene, Square\n\nclass Demo(Scene):\n    def construct(self):\n        square = Square()\n        square.shift(LEFT)\n        square.shift(LEFT * 2)\n        self.add(square)\n";
    let directory = project(source);
    let facts = facts(directory.path());
    let object = target_object(&facts.document, "square");
    let request = object_request(
        &facts.document,
        &object,
        &json!({ "kind": "modify-existing-shift", "argument": "RIGHT" }),
    );
    let report = run(directory.path(), &request);
    assert_eq!(report.document["status"], "ambiguous");
    assert_eq!(report.document["candidates"].as_array().unwrap().len(), 2);
    assert!(
        report.document["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|candidate| candidate["confidence"] == "medium"
                && candidate["validation"]["status"] == "accepted"
                && candidate["validation"]["rematch"]["status"] == "match")
    );
    assert_eq!(
        std::fs::read_to_string(directory.path().join("scene.py")).unwrap(),
        source
    );
}

#[test]
fn existing_shift_search_stays_in_the_allocation_context() {
    let source = "from manim import LEFT, RIGHT, Scene, Square\n\nclass Demo(Scene):\n    def unrelated(self):\n        square = Square()\n        square.shift(LEFT)\n\n    def construct(self):\n        square = Square()\n        square.shift(LEFT)\n        self.add(square)\n";
    let directory = project(source);
    let facts = facts(directory.path());
    let object = target_object(&facts.document, "square");
    let request = object_request(
        &facts.document,
        &object,
        &json!({ "kind": "modify-existing-shift", "argument": "RIGHT" }),
    );
    let report = run(directory.path(), &request);
    assert_eq!(report.document["status"], "unique", "{}", report.output);
    let candidate = &report.document["candidates"][0];
    assert_eq!(candidate["confidence"], "high");
    assert_eq!(candidate["edits"][0]["original_text"], "LEFT");
    let edit_start = candidate["edits"][0]["anchor"]["utf8_byte_range"]["start"]
        .as_u64()
        .unwrap();
    let construct_start = source.find("    def construct").unwrap() as u64;
    assert!(edit_start > construct_start);
}

#[test]
fn replaces_only_a_statically_literal_argument() {
    let source = "from manim import Scene, Square\n\nclass Demo(Scene):\n    def construct(self):\n        square = Square(1)\n        self.add(square)\n";
    let directory = project(source);
    let facts = facts(directory.path());
    let object = target_object(&facts.document, "square");
    let allocation = &object["allocation_anchor"];
    let request = object_request(
        &facts.document,
        &object,
        &json!({
            "kind": "replace-literal-argument",
            "call": {
                "path": allocation["path"],
                "raw_content_hash": allocation["raw_content_hash"],
                "utf8_byte_range": allocation["utf8_byte_range"],
            },
            "argument": { "position": 0 },
            "replacement": "12",
        }),
    );
    let report = run(directory.path(), &request);
    assert_eq!(report.document["status"], "unique");
    assert_eq!(
        report.document["candidates"][0]["edits"][0]["original_text"],
        "1"
    );
    assert_eq!(
        report.document["candidates"][0]["validation"]["rematch"]["status"],
        "match"
    );
}

#[test]
fn replaces_a_statically_literal_keyword_argument() {
    let source = "from manim import Scene, Square\n\nclass Demo(Scene):\n    def construct(self):\n        square = Square(side_length=1)\n        self.add(square)\n";
    let directory = project(source);
    let facts = facts(directory.path());
    let object = target_object(&facts.document, "square");
    let allocation = &object["allocation_anchor"];
    let request = object_request(
        &facts.document,
        &object,
        &json!({
            "kind": "replace-literal-argument",
            "call": {
                "path": allocation["path"],
                "raw_content_hash": allocation["raw_content_hash"],
                "utf8_byte_range": allocation["utf8_byte_range"],
            },
            "argument": { "keyword": "side_length" },
            "replacement": "2.5",
        }),
    );
    let report = run(directory.path(), &request);
    assert_eq!(report.document["status"], "unique", "{}", report.output);
    assert_eq!(
        report.document["candidates"][0]["edits"][0]["original_text"],
        "1"
    );
    assert_eq!(
        report.document["candidates"][0]["edits"][0]["replacement"],
        "2.5"
    );
}

#[test]
fn literal_replacement_refuses_an_unrelated_target_call() {
    let source = "from manim import Circle, Scene, Square\n\nclass Demo(Scene):\n    def construct(self):\n        square = Square(1)\n        circle = Circle(2)\n        self.add(square, circle)\n";
    let directory = project(source);
    let facts = facts(directory.path());
    let square = target_object(&facts.document, "square");
    let circle = target_object(&facts.document, "circle");
    let unrelated = &circle["allocation_anchor"];
    let request = object_request(
        &facts.document,
        &square,
        &json!({
            "kind": "replace-literal-argument",
            "call": {
                "path": unrelated["path"],
                "raw_content_hash": unrelated["raw_content_hash"],
                "utf8_byte_range": unrelated["utf8_byte_range"],
            },
            "argument": { "position": 0 },
            "replacement": "3",
        }),
    );
    let report = run(directory.path(), &request);
    assert_eq!(report.document["status"], "unavailable");
    assert_eq!(
        report.document["unknowns"][0]["reasons"][0]["kind"],
        "call-not-target-anchor"
    );
    assert!(schema_errors(OUTPUT_SCHEMA, &report.document).is_empty());
}

#[test]
fn rematching_adjusts_a_later_helper_call_context() {
    let source = "from manim import RIGHT, Scene, Square\n\ndef make_square():\n    return Square()\n\nclass Demo(Scene):\n    def construct(self):\n        square = make_square()\n        self.add(square)\n";
    let directory = project(source);
    let facts = facts(directory.path());
    let object = target_object(&facts.document, "square");
    assert_eq!(object["call_context"].as_array().unwrap().len(), 1);
    let request = object_request(
        &facts.document,
        &object,
        &json!({ "kind": "insert-shift-chain", "argument": "RIGHT" }),
    );
    let report = run(directory.path(), &request);
    assert_eq!(report.document["status"], "unique", "{}", report.output);
    assert_eq!(
        report.document["candidates"][0]["validation"]["rematch"]["status"],
        "match"
    );
}

#[test]
fn hash_mismatch_and_dynamic_literal_are_reasoned_unavailable() {
    let source = "from manim import Scene, Square\n\nclass Demo(Scene):\n    def construct(self):\n        size = 1\n        square = Square(size)\n        self.add(square)\n";
    let directory = project(source);
    let facts = facts(directory.path());
    let object = target_object(&facts.document, "square");
    let allocation = &object["allocation_anchor"];
    let dynamic = object_request(
        &facts.document,
        &object,
        &json!({
            "kind": "replace-literal-argument",
            "call": {
                "path": allocation["path"],
                "raw_content_hash": allocation["raw_content_hash"],
                "utf8_byte_range": allocation["utf8_byte_range"],
            },
            "argument": { "position": 0 },
            "replacement": "2",
        }),
    );
    let report = run(directory.path(), &dynamic);
    assert_eq!(report.document["status"], "unavailable");
    assert_eq!(
        report.document["unknowns"][0]["reasons"][0]["kind"],
        "dynamic-existing-argument"
    );

    let bad_hash = object_request(
        &facts.document,
        &object,
        &json!({
            "kind": "replace-literal-argument",
            "call": {
                "path": allocation["path"],
                "raw_content_hash": format!("sha256:{}", "0".repeat(64)),
                "utf8_byte_range": allocation["utf8_byte_range"],
            },
            "argument": { "position": 0 },
            "replacement": "2",
        }),
    );
    let report = run(directory.path(), &bad_hash);
    assert_eq!(report.document["status"], "unavailable");
    assert_eq!(
        report.document["unknowns"][0]["reasons"][0]["kind"],
        "source-precondition-failed"
    );
}

#[test]
fn new_dynamic_frontier_rejects_an_otherwise_parseable_patch() {
    let directory = project(SIMPLE);
    let facts = facts(directory.path());
    let object = target_object(&facts.document, "square");
    let request = object_request(
        &facts.document,
        &object,
        &json!({ "kind": "insert-shift-chain", "argument": "unknown_vector()" }),
    );
    let report = run(directory.path(), &request);
    assert_eq!(report.document["status"], "unavailable");
    let validation = &report.document["candidates"][0]["validation"];
    assert_eq!(validation["parse"], "valid");
    assert_eq!(validation["coverage"]["status"], "decreased");
    assert!(
        validation["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason["kind"] == "coverage-decreased")
    );
}

#[test]
fn schemas_reject_unknown_fields_and_reasonless_unknowns() {
    let directory = project(SIMPLE);
    let facts = facts(directory.path());
    let object = target_object(&facts.document, "square");
    let mut request = object_request(
        &facts.document,
        &object,
        &json!({ "kind": "modify-existing-shift", "argument": "RIGHT" }),
    );
    request["execute"] = Value::Bool(true);
    assert!(!schema_errors(REQUEST_SCHEMA, &request).is_empty());

    let unavailable = object_request(
        &facts.document,
        &object,
        &json!({ "kind": "modify-existing-shift", "argument": "RIGHT" }),
    );
    let report = run(directory.path(), &unavailable);
    assert!(schema_errors(OUTPUT_SCHEMA, &report.document).is_empty());
    let mut invalid = report.document;
    invalid["unknowns"][0]["reasons"] = Value::Array(Vec::new());
    assert!(!schema_errors(OUTPUT_SCHEMA, &invalid).is_empty());
}

#[test]
fn command_rejects_requests_outside_the_published_schema() {
    let directory = project(SIMPLE);
    let request_file = directory.path().join("request.json");
    std::fs::write(
        &request_file,
        serde_json::to_vec(&json!({
            "schema_version": 0,
            "snapshot_id": format!("snapshot:sf0:{}", "0".repeat(64)),
            "target_id": "not-an-entity-id",
            "operation": { "kind": "insert-shift-chain", "argument": "RIGHT" },
        }))
        .unwrap(),
    )
    .unwrap();
    let error = source_bridge(&SourceBridgeArgs {
        path: directory.path().to_path_buf(),
        request: request_file,
        profile: None,
        renderer: None,
        fps: None,
        resolution: None,
    })
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("target_id is not a StaticFacts v0 entity ID")
    );
}

#[test]
fn output_is_byte_identical_across_runs_and_worker_counts() {
    let directory = project(SIMPLE);
    let facts = facts(directory.path());
    let object = target_object(&facts.document, "square");
    let request = object_request(
        &facts.document,
        &object,
        &json!({ "kind": "insert-shift-chain", "argument": "RIGHT * 2" }),
    );
    let request_file = directory.path().join("request.json");
    std::fs::write(&request_file, serde_json::to_vec_pretty(&request).unwrap()).unwrap();
    let run_with = |workers| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .build()
            .unwrap()
            .install(|| {
                source_bridge(&SourceBridgeArgs {
                    path: directory.path().to_path_buf(),
                    request: request_file.clone(),
                    profile: None,
                    renderer: None,
                    fps: None,
                    resolution: None,
                })
                .unwrap()
                .output
            })
    };
    assert_eq!(run_with(1), run_with(4));
    assert_eq!(run_with(4), run_with(4));
}

#[test]
fn shift_jis_edit_validation_preserves_encoding_and_source_bytes() {
    let decoded = "# coding: shift_jis\nfrom pathlib import Path\nPath('EXECUTED').write_text('bad')\nraise RuntimeError('must not run')\nfrom manim import RIGHT, Scene, Square\n\nclass 日本(Scene):\n    def construct(self):\n        四角 = Square()\n        self.add(四角)\n";
    let (encoded, _, errors) = encoding_rs::SHIFT_JIS.encode(decoded);
    assert!(!errors);
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("シーン.py"), encoded.as_ref()).unwrap();
    let original = std::fs::read(directory.path().join("シーン.py")).unwrap();
    let facts = facts(directory.path());
    let object = target_object(&facts.document, "四角");
    let request = object_request(
        &facts.document,
        &object,
        &json!({ "kind": "insert-shift-chain", "argument": "RIGHT" }),
    );
    let report = run(directory.path(), &request);
    assert_eq!(report.document["status"], "unique");
    assert_eq!(
        report.document["candidates"][0]["edits"][0]["anchor"]["encoding"],
        "shift_jis"
    );
    assert_eq!(
        report.document["candidates"][0]["validation"]["rematch"]["status"],
        "match"
    );
    assert_eq!(
        std::fs::read(directory.path().join("シーン.py")).unwrap(),
        original
    );
    assert!(!directory.path().join("EXECUTED").exists());
    assert!(schema_errors(OUTPUT_SCHEMA, &report.document).is_empty());
}
