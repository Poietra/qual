//! Phase 3 performance rule golden tests, timing / display-order tranche
//! (DESIGN §11.1, §7.3): `MLP209`, `MLP210`, `MLP215`, `MLP218`,
//! `MLP219`, `MLP221`, `MLP222`, `MLP227`.
//!
//! Each rule fixture directory holds `invalid.py` (true positives),
//! `valid.py` (near misses that must stay silent), `branch.py` (an
//! Unknown / Maybe case that must silence or lower confidence), and
//! `suppressed.py` (an inline suppression that must win). The golden
//! expectations pin the exact rule, file, line, column, severity, and
//! confidence of every diagnostic the whole directory produces.

use std::path::{Path, PathBuf};

use manim_lint::application::check;
use manim_lint::cli::CheckArgs;
use manim_lint::reporting::OutputFormat;

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
fn mlp209_front_loaded_static_suffix_golden() {
    // `valid.py` (the updater-bearing object sits last: empty static
    // suffix) and `branch.py` (a non-literal set_z_index poisons the
    // display order: no quantitative claim) stay silent.
    assert_golden("MLP209", &["invalid.py:14:9 MLP209 info high"]);
}

#[test]
fn mlp210_fixed_count_loop_plays_golden() {
    // `valid.py` (three iterations; a play behind an `if`) and
    // `branch.py` (a non-literal bound; a loop with `break`) stay silent.
    assert_golden("MLP210", &["invalid.py:8:13 MLP210 info medium"]);
}

#[test]
fn mlp215_no_op_updater_golden() {
    // `valid.py` is the binding catalog near miss: a one-argument no-op
    // updater does not dynamicize a plain wait, and with no play there is
    // no moving scope either. `branch.py` (registration on one path only)
    // stays silent.
    assert_golden(
        "MLP215",
        &[
            "invalid.py:8:9 MLP215 warning high",
            "invalid.py:17:9 MLP215 warning high",
        ],
    );
}

#[test]
fn mlp218_frame_invariant_dynamic_wait_golden() {
    // `valid.py` (a dt-scaled updater; an accumulator reading its own
    // target) and `branch.py` (registration on one path only) stay
    // silent.
    assert_golden("MLP218", &["invalid.py:9:9 MLP218 info high"]);
}

#[test]
fn mlp219_long_lived_updater_golden() {
    // `valid.py` (clear_updaters after the first play; a span below the
    // gates) and `branch.py` (registration on one path only) stay silent.
    assert_golden("MLP219", &["invalid.py:13:9 MLP219 info medium"]);
}

#[test]
fn mlp221_excessive_sample_count_golden() {
    // `valid.py` (sane literal steps; a two-element range) and
    // `branch.py` (a non-literal range element) stay silent.
    assert_golden(
        "MLP221",
        &[
            "invalid.py:8:17 MLP221 warning high",
            "invalid.py:10:17 MLP221 warning high",
        ],
    );
}

#[test]
fn mlp222_image_in_moving_suffix_golden() {
    // `valid.py` (the image draws before the first moving member: it
    // stays in the static base) and `branch.py` (a non-literal
    // set_z_index leaves the order unknown) stay silent.
    assert_golden("MLP222", &["invalid.py:7:17 MLP222 warning high"]);
}

#[test]
fn mlp227_forced_always_update_wait_golden() {
    // `valid.py` (a real time-based updater; no flag at all) and
    // `branch.py` (a non-literal flag value: Maybe never fires) stay
    // silent.
    assert_golden("MLP227", &["invalid.py:9:9 MLP227 warning high"]);
}
