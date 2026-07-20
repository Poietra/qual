//! Contract tests for RFC 0001 and `schemas/static-facts-v0.json`.

use serde_json::Value;

const SCHEMA: &str = include_str!("../schemas/static-facts-v0.json");
const REPRESENTATIVE: &str = include_str!("fixtures/static-facts-v0/representative.json");

fn parse_schema() -> Value {
    serde_json::from_str(SCHEMA).expect("StaticFacts v0 schema must be valid JSON")
}

fn parse_representative() -> Value {
    serde_json::from_str(REPRESENTATIVE).expect("representative facts must be valid JSON")
}

fn assert_valid(validator: &jsonschema::Validator, instance: &Value) {
    let errors: Vec<_> = validator
        .iter_errors(instance)
        .map(|error| error.to_string())
        .collect();
    assert!(errors.is_empty(), "schema errors:\n{}", errors.join("\n"));
}

#[test]
fn schema_is_valid_draft_2020_12() {
    let schema = parse_schema();
    jsonschema::meta::options()
        .validate(&schema)
        .expect("StaticFacts schema must satisfy its declared meta-schema");
}

#[test]
fn representative_document_validates() {
    let schema = parse_schema();
    let validator = jsonschema::validator_for(&schema).expect("compile schema");
    assert_valid(&validator, &parse_representative());
}

#[test]
fn representative_covers_identity_and_encoding_decisions() {
    let facts = parse_representative();
    let files = facts["files"].as_array().unwrap();
    assert_eq!(files[0]["path"], "シーン.py");
    assert_eq!(files[0]["encoding"], "shift_jis");

    let objects = facts["objects"].as_array().unwrap();
    assert_eq!(
        objects[0]["allocation_anchor"],
        objects[1]["allocation_anchor"]
    );
    assert_ne!(objects[0]["call_context"], objects[1]["call_context"]);
    assert_ne!(objects[0]["id"], objects[1]["id"]);
    assert_eq!(objects[2]["cardinality"], "many");

    let animations = facts["animations"].as_array().unwrap();
    assert_eq!(
        animations[0]["kind_candidates"]["values"][0],
        "manim.animation.transform.Transform"
    );
    assert_eq!(
        animations[1]["kind_candidates"]["values"][0],
        "manim.animation.transform.ReplacementTransform"
    );
    assert_eq!(
        animations[1]["target_candidates"]["reasons"][0]["kind"],
        "dynamic-call-target"
    );
}

#[test]
fn unknown_without_reasons_is_rejected() {
    let schema = parse_schema();
    let validator = jsonschema::validator_for(&schema).expect("compile schema");
    let mut facts = parse_representative();
    facts["animations"][1]["target_candidates"]
        .as_object_mut()
        .unwrap()
        .remove("reasons");
    assert!(!validator.is_valid(&facts));
}

#[test]
fn optimization_permission_and_unknown_fields_are_rejected() {
    let schema = parse_schema();
    let validator = jsonschema::validator_for(&schema).expect("compile schema");
    let mut facts = parse_representative();
    facts["renderer_risks"][0]
        .as_object_mut()
        .unwrap()
        .insert("safe_to_skip_render".into(), Value::Bool(true));
    assert!(!validator.is_valid(&facts));
}

#[test]
fn scene_updater_host_is_supported() {
    let schema = parse_schema();
    let validator = jsonschema::validator_for(&schema).expect("compile schema");
    let mut facts = parse_representative();
    let scene_id = facts["scenes"][0]["id"].clone();
    facts["updaters"][0]["host_candidates"]["entity_ids"][0] = scene_id;
    assert_valid(&validator, &facts);
}

#[test]
fn coverage_completeness_must_match_frontiers() {
    let schema = parse_schema();
    let validator = jsonschema::validator_for(&schema).expect("compile schema");
    let mut facts = parse_representative();
    facts["coverage"]["completeness"] = Value::String("complete".into());
    assert!(!validator.is_valid(&facts));

    facts["coverage"]["completeness"] = Value::String("partial".into());
    facts["coverage"]["frontiers"] = Value::Array(Vec::new());
    assert!(!validator.is_valid(&facts));
}
