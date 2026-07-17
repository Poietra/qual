//! Phase 1 lifecycle rule golden tests (DESIGN §11.1).
//!
//! Each rule fixture directory holds `invalid.py` (true positives),
//! `valid.py` (near misses that must stay silent), `alias.py` (the same
//! true positive expressed through `import manim as mn`, proving import
//! style does not change results), and `suppressed.py` (an inline
//! suppression that must win). The golden expectations pin the exact rule,
//! file, line, column, severity, and confidence of every diagnostic the
//! whole directory produces; `valid.py` and `suppressed.py` are silent by
//! virtue of not appearing.

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

/// Copies every fixture file of one rule into a fresh temp project so fix
/// application never mutates the checked-in fixtures.
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
    // Import style must not change results: the alias fixture reproduces
    // the star-import true positive through `import manim as mn`.
    assert!(
        rows.iter()
            .any(|row| row.starts_with("alias.py") && row.contains(rule)),
        "{rule}: alias-import variant must fire identically"
    );
}

#[test]
fn mlc101_empty_play_golden() {
    assert_golden(
        "MLC101",
        &[
            "alias.py:6:9 MLC101 error certain",
            "invalid.py:6:9 MLC101 error certain",
            "invalid.py:7:9 MLC101 error certain",
        ],
    );
}

#[test]
fn mlc102_non_animation_play_argument_golden() {
    assert_golden(
        "MLC102",
        &[
            "alias.py:7:19 MLC102 error high",
            "invalid.py:7:19 MLC102 error high",
            "invalid.py:8:19 MLC102 error high",
            "invalid.py:9:19 MLC102 error high",
            "invalid.py:10:19 MLC102 error high",
            "invalid.py:11:19 MLC102 error high",
        ],
    );
}

#[test]
fn mlc103_bound_method_play_argument_golden() {
    assert_golden(
        "MLC103",
        &[
            "alias.py:7:19 MLC103 error certain",
            "invalid.py:7:19 MLC103 error certain",
            "invalid.py:8:19 MLC103 error certain",
        ],
    );
}

#[test]
fn mlc104_non_positive_duration_golden() {
    assert_golden(
        "MLC104",
        &[
            "alias.py:6:19 MLC104 error certain",
            "invalid.py:7:44 MLC104 error certain",
            "invalid.py:8:44 MLC104 error certain",
            "invalid.py:9:19 MLC104 error certain",
            "invalid.py:10:19 MLC104 error certain",
            "invalid.py:11:28 MLC104 error certain",
            "invalid.py:12:33 MLC104 error certain",
        ],
    );
}

#[test]
fn mlc105_updater_cannot_bind_golden() {
    assert_golden(
        "MLC105",
        &[
            "alias.py:7:28 MLC105 error high",
            "invalid.py:11:28 MLC105 error high",
            "invalid.py:12:28 MLC105 error high",
            "invalid.py:13:28 MLC105 error high",
            "invalid.py:14:28 MLC105 error high",
            "invalid.py:15:26 MLC105 error high",
        ],
    );
}

#[test]
fn mlc106_frozen_frame_stop_condition_golden() {
    assert_golden(
        "MLC106",
        &[
            "alias.py:6:61 MLC106 error certain",
            "invalid.py:8:61 MLC106 error certain",
        ],
    );
}

#[test]
fn mlc109_empty_animation_group_golden() {
    assert_golden(
        "MLC109",
        &[
            "alias.py:6:17 MLC109 error certain",
            "invalid.py:6:17 MLC109 error certain",
            "invalid.py:7:17 MLC109 error certain",
        ],
    );
}

#[test]
fn mlc122_apply_method_call_result_golden() {
    assert_golden(
        "MLC122",
        &[
            "alias.py:7:34 MLC122 error high",
            "invalid.py:7:31 MLC122 error high",
        ],
    );
}

#[test]
fn mlc126_invalid_family_child_golden() {
    assert_golden(
        "MLC126",
        &[
            "alias.py:7:27 MLC126 error high",
            "invalid.py:9:32 MLC126 error high",
            "invalid.py:10:19 MLC126 error high",
            "invalid.py:11:19 MLC126 error high",
            "invalid.py:12:23 MLC126 error high",
            "invalid.py:13:24 MLC126 error high",
        ],
    );
}

#[test]
fn mlc127_duplicate_child_golden() {
    assert_golden(
        "MLC127",
        &[
            "alias.py:7:35 MLC127 info certain",
            "invalid.py:8:37 MLC127 info certain",
            "invalid.py:9:27 MLC127 info certain",
            "invalid.py:10:24 MLC127 info certain",
        ],
    );
}

#[test]
fn mlc127_safe_fix_produces_valid_python_and_is_idempotent() {
    let project = copy_fixture("MLC127");
    let mut fix_args = args_for(project.path());
    fix_args.fix = true;

    let report = check(&fix_args).unwrap();
    let fixes = report.fixes.expect("--fix must produce a report");
    assert_eq!(fixes.applied, 4, "all duplicate-child fixes are SAFE");
    assert!(fixes.rolled_back.is_empty(), "fixed text must re-parse");

    // The fixed project is clean: the duplicates are gone and every fixed
    // file still parses (no MLC000).
    let after = observed(project.path());
    assert_eq!(after, Vec::<String>::new(), "fixed project must be clean");

    // Second pass is a no-op (idempotence, DESIGN §11.2).
    let second = check(&fix_args).unwrap();
    assert_eq!(second.fixes.expect("fix report").applied, 0);

    let rewritten = std::fs::read_to_string(project.path().join("invalid.py")).unwrap();
    assert!(rewritten.contains("group = VGroup(square, dot)"));
    assert!(rewritten.contains("band = Group(dot)"));
    assert!(rewritten.contains("group.add(dot)"));
}

#[test]
fn unsafe_fixes_rewrite_to_the_suggested_api() {
    // MLC102: `mob.method(...)` becomes `mob.animate.method(...)`.
    let project = copy_fixture("MLC102");
    let mut fix_args = args_for(project.path());
    fix_args.fix = true;
    fix_args.unsafe_fixes = true;
    let report = check(&fix_args).unwrap();
    assert!(report.fixes.expect("fix report").rolled_back.is_empty());
    let fixed = std::fs::read_to_string(project.path().join("invalid.py")).unwrap();
    assert!(fixed.contains("self.play(square.animate.shift(RIGHT))"));

    // MLC106: `frozen_frame=True` becomes `frozen_frame=False`.
    let project = copy_fixture("MLC106");
    let mut fix_args = args_for(project.path());
    fix_args.fix = true;
    fix_args.unsafe_fixes = true;
    check(&fix_args).unwrap();
    let fixed = std::fs::read_to_string(project.path().join("invalid.py")).unwrap();
    assert!(fixed.contains("frozen_frame=False"));
    assert!(observed(project.path()).is_empty());

    // MLC122: the call result is split back into method plus arguments.
    let project = copy_fixture("MLC122");
    let mut fix_args = args_for(project.path());
    fix_args.fix = true;
    fix_args.unsafe_fixes = true;
    check(&fix_args).unwrap();
    let fixed = std::fs::read_to_string(project.path().join("invalid.py")).unwrap();
    assert!(fixed.contains("self.play(ApplyMethod(square.shift, RIGHT))"));
    assert!(observed(project.path()).is_empty());
}

#[test]
fn lifecycle_rule_metadata_matches_the_design_catalog() {
    let expected: [(&str, Severity, Confidence); 10] = [
        ("MLC101", Severity::Error, Confidence::Certain),
        ("MLC102", Severity::Error, Confidence::High),
        ("MLC103", Severity::Error, Confidence::Certain),
        ("MLC104", Severity::Error, Confidence::Certain),
        ("MLC105", Severity::Error, Confidence::High),
        ("MLC106", Severity::Error, Confidence::Certain),
        ("MLC109", Severity::Error, Confidence::Certain),
        ("MLC122", Severity::Error, Confidence::High),
        ("MLC126", Severity::Error, Confidence::High),
        ("MLC127", Severity::Info, Confidence::Certain),
    ];
    for (rule, severity, confidence) in expected {
        let metadata = registry::metadata_for(rule)
            .unwrap_or_else(|| panic!("{rule} must be registered as implemented"));
        assert!(metadata.default_enabled, "{rule} defaults to enabled");
        assert_eq!(metadata.default_severity, severity, "{rule} severity");
        assert_eq!(metadata.minimum_confidence, confidence, "{rule} confidence");
        assert_eq!(metadata.implementation_phase, 1, "{rule} phase");
        assert_eq!(
            metadata.required_capabilities,
            ["qualified-calls"],
            "{rule} capabilities"
        );
    }
}
