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
        format: OutputFormat::Json,
        ..CheckArgs::default()
    }
}

/// Runs `check` over a rule fixture directory and renders each diagnostic
/// as `path:line:column rule severity confidence`, in pipeline order.
fn observed(root: &Path) -> Vec<String> {
    let report = check(&args_for(root)).unwrap();
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
    // `valid.py` (cold constructions, DecimalNumber + set_value, cold
    // helper) and `branch.py` (a callback variable whose identity cannot
    // be resolved statically) stay silent.
    assert_golden(
        "MLP201",
        &[
            "invalid.py:7:47 MLP201 warning high",
            "invalid.py:8:39 MLP201 warning high",
        ],
    );
}

#[test]
fn mlp204_hot_scene_graph_growth_golden() {
    // `valid.py` (re-adding an existing object: reorder-only) and
    // `branch.py` (a helper call whose freshness is unproven) stay silent.
    assert_golden(
        "MLP204",
        &[
            "invalid.py:7:37 MLP204 warning high",
            "invalid.py:8:38 MLP204 warning high",
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
    // The frame-varying f-string key carries only MLP226 (it supersedes
    // MLP201 on that span); the static-key sibling construction carries
    // MLP201; `branch.py` (string concatenation is not a proven
    // frame-varying key) falls back to MLP201.
    assert_golden(
        "MLP226",
        &[
            "branch.py:7:39 MLP201 warning high",
            "invalid.py:7:39 MLP226 warning high",
            "invalid.py:8:40 MLP201 warning high",
        ],
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
    // from the literal play duration only.
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
fn performance_rule_metadata_matches_the_design_catalog() {
    let expected: [(&str, Severity, Confidence, &[&str]); 6] = [
        ("MLP201", Severity::Warning, Confidence::High, &[]),
        ("MLP204", Severity::Warning, Confidence::High, &[]),
        ("MLP205", Severity::Warning, Confidence::High, &[]),
        ("MLP206", Severity::Warning, Confidence::Certain, &[]),
        (
            "MLP220",
            Severity::Warning,
            Confidence::High,
            &["MLP204", "MLP211"],
        ),
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
    // Unimplemented catalog ids of this tranche stay reserved and are
    // never reported as checked (DESIGN §12).
    for reserved in ["MLP217", "MLP218", "MLP221", "MLP225", "MLP227"] {
        assert!(
            registry::metadata_for(reserved).is_none(),
            "{reserved} must stay unimplemented"
        );
        assert!(
            registry::is_reserved_rule_id(reserved),
            "{reserved} keeps its reserved id"
        );
    }
}
