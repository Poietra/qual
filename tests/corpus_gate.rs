//! Labeled-corpus release gate (DESIGN §11.4).
//!
//! `tests/corpus/manifest-v1.json` pins, for every corpus case: the
//! relative source path, the sha256 of the case source, a label revision,
//! and the exact expected diagnostics (rule / line / column / severity /
//! confidence) under the default configuration, each classified as a
//! true positive or a false-positive guard (`expected: []`).
//!
//! This gate runs the real `check` pipeline over every case and asserts
//! byte-level agreement with the labels. A sha256 mismatch is a *label
//! integrity* failure, not a formatting one: the case must be
//! re-adjudicated (CONTRIBUTING.md, "Corpus labeling"), never re-recorded.

use std::path::{Path, PathBuf};

use manim_lint::application::check;
use manim_lint::cli::CheckArgs;
use manim_lint::knowledge::generator::sha256_hex;
use manim_lint::reporting::OutputFormat;

fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus")
}

fn manifest() -> serde_json::Value {
    let path = corpus_root().join("manifest-v1.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{} is not valid JSON: {error}", path.display()))
}

/// One expected diagnostic rendered exactly like the golden-test rows.
fn expected_row(value: &serde_json::Value) -> String {
    format!(
        "{rule} {line}:{column} {severity} {confidence}",
        rule = value["rule"].as_str().expect("expected.rule"),
        line = value["line"].as_u64().expect("expected.line"),
        column = value["column"].as_u64().expect("expected.column"),
        severity = value["severity"].as_str().expect("expected.severity"),
        confidence = value["confidence"].as_str().expect("expected.confidence"),
    )
}

/// Runs the real check pipeline over one case source in a fresh temp
/// project (so repository-level configuration can never leak in) and
/// renders each diagnostic in the same shape as `expected_row`.
fn observed_rows(case_path: &Path) -> Vec<String> {
    let project = tempfile::tempdir().expect("temp project");
    let file_name = case_path.file_name().expect("case file name");
    std::fs::copy(case_path, project.path().join(file_name)).expect("copy case");
    let args = CheckArgs {
        paths: vec![project.path().to_path_buf()],
        format: OutputFormat::Json,
        ..CheckArgs::default()
    };
    let report = check(&args).expect("check pipeline");
    report
        .diagnostics
        .iter()
        .map(|diagnostic| {
            format!(
                "{rule} {line}:{column} {severity} {confidence}",
                rule = diagnostic.rule_id,
                line = diagnostic.primary_span.start.line,
                column = diagnostic.primary_span.start.column,
                severity = diagnostic.severity,
                confidence = diagnostic.confidence,
            )
        })
        .collect()
}

/// DESIGN §11.4: the manifest itself must stay a meaningful release gate —
/// enough cases, all four rule families, coherent per-case labels.
#[test]
fn corpus_manifest_is_structurally_sound() {
    let manifest = manifest();
    assert_eq!(manifest["schema_version"], 1, "manifest schema version");
    let cases = manifest["cases"].as_array().expect("cases array");
    assert!(
        cases.len() >= 25,
        "the corpus gate needs at least 25 labeled cases, found {}",
        cases.len()
    );

    let mut families: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut seen_paths: Vec<&str> = Vec::new();
    for case in cases {
        let path = case["path"].as_str().expect("case.path");
        assert!(
            !seen_paths.contains(&path),
            "duplicate corpus case path {path}"
        );
        seen_paths.push(path);
        assert!(
            case["label_revision"].as_u64().is_some_and(|rev| rev >= 1),
            "{path}: label_revision must be a positive integer"
        );
        let classification = case["classification"].as_str().expect("classification");
        let expected = case["expected"].as_array().expect("expected array");
        match classification {
            "true-positive" | "mixed" => assert!(
                !expected.is_empty(),
                "{path}: a {classification} case must expect diagnostics"
            ),
            "false-positive-guard" => assert!(
                expected.is_empty(),
                "{path}: a false-positive guard must expect silence"
            ),
            other => panic!("{path}: unknown classification {other}"),
        }
        for diagnostic in expected {
            let rule = diagnostic["rule"].as_str().expect("expected.rule");
            assert_eq!(
                diagnostic["classification"], "true-positive",
                "{path}: every expected diagnostic must be an adjudicated \
                 true positive"
            );
            if let Some(family) = rule.get(..3) {
                families.insert(family.to_owned());
            }
        }
    }
    for family in ["MLC", "MLR", "MLP", "MLD"] {
        assert!(
            families.contains(family),
            "the corpus must cover rule family {family} with at least one \
             expected diagnostic"
        );
    }
}

/// The gate proper: sources match their pinned sha256 and every case
/// produces exactly its labeled diagnostics.
#[test]
fn corpus_cases_produce_exactly_their_labeled_diagnostics() {
    let manifest = manifest();
    let root = corpus_root();
    let mut failures: Vec<String> = Vec::new();

    for case in manifest["cases"].as_array().expect("cases array") {
        let relative = case["path"].as_str().expect("case.path");
        let case_path = root.join(relative);
        let source = std::fs::read(&case_path)
            .unwrap_or_else(|error| panic!("cannot read corpus case {relative}: {error}"));

        // Label-revision safety: the labels describe exactly this source.
        let digest = sha256_hex(&source);
        let pinned = case["sha256"].as_str().expect("case.sha256");
        assert_eq!(
            digest,
            pinned,
            "corpus case {relative} does not match its manifest sha256 \
             (label revision {revision}).\n\
             The source changed after its labels were adjudicated. Do NOT \
             just re-record the observed diagnostics: re-adjudicate the \
             case following CONTRIBUTING.md 'Corpus labeling' (verify each \
             expected diagnostic by hand against Manim semantics), then \
             bump label_revision and update sha256 in the same change.",
            revision = case["label_revision"],
        );

        let expected: Vec<String> = case["expected"]
            .as_array()
            .expect("expected array")
            .iter()
            .map(expected_row)
            .collect();
        let observed = observed_rows(&case_path);
        if observed != expected {
            failures.push(format!(
                "{relative} (label revision {revision}):\n  expected: {expected:?}\n  observed: {observed:?}",
                revision = case["label_revision"],
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "corpus cases diverged from their adjudicated labels — if the new \
         behavior is intentional, re-adjudicate each diagnostic \
         (CONTRIBUTING.md 'Corpus labeling') and bump the case's \
         label_revision:\n\n{}",
        failures.join("\n\n")
    );
}

/// Determinism (DESIGN §15 invariant 12): the same corpus input renders a
/// byte-identical JSON report across two independent pipeline runs.
#[test]
fn corpus_check_output_is_byte_stable() {
    let root = corpus_root();
    let basic = root.join("cases/manim_example_scenes/basic.py");
    let render = || {
        let project = tempfile::tempdir().expect("temp project");
        std::fs::copy(&basic, project.path().join("basic.py")).expect("copy case");
        let args = CheckArgs {
            paths: vec![project.path().to_path_buf()],
            format: OutputFormat::Json,
            ..CheckArgs::default()
        };
        check(&args).expect("check pipeline").output
    };
    assert_eq!(render(), render(), "JSON output must be byte-stable");
}
