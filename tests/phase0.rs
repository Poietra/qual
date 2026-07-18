//! Phase 0 integration tests (DESIGN §11.2 layers 1, 6, 7 and the Phase 0
//! acceptance criterion): multiple files are analyzed with stable output,
//! a syntax error in one file never stops the others, and the JSON output
//! matches the v1 schema requirements.

use std::path::Path;

use manim_lint::application::{self, check};
use manim_lint::cli::{CheckArgs, Command, ExitStatus};
use manim_lint::diagnostic::Severity;
use manim_lint::reporting::OutputFormat;

const PYPROJECT: &str = r#"
[tool.manim-lint]
select = ["MLC", "MLR", "MLP", "MLD"]
min-confidence = "high"
fail-level = "warning"
default-profile = "production"

[[tool.manim-lint.profile]]
name = "production"
renderer = "cairo"
pixel-width = 1920
pixel-height = 1080
frame-rate = 30
"#;

/// Valid scene; its unknown suppression ID proves the file was analyzed.
const GOOD_SCENE: &str = "\
\"\"\"Valid scene.\"\"\"

value = 1  # manim-lint: ignore[MLC999]
suppressed = 2  # manim-lint: ignore[MLC108]
";

/// Japanese text before the syntax error exercises character columns.
const BAD_SCENE: &str = "x = 1\ny = \"こんにちは\"; def = 1\n";

fn write_project(root: &Path) {
    std::fs::write(root.join("pyproject.toml"), PYPROJECT).unwrap();
    std::fs::create_dir_all(root.join("scenes")).unwrap();
    std::fs::write(root.join("scenes/good.py"), GOOD_SCENE).unwrap();
    std::fs::write(root.join("scenes/bad.py"), BAD_SCENE).unwrap();
    std::fs::create_dir_all(root.join(".venv")).unwrap();
    std::fs::write(root.join(".venv/skip.py"), "this is not python (\n").unwrap();
}

fn args_for(root: &Path, format: OutputFormat) -> CheckArgs {
    CheckArgs {
        paths: vec![root.to_path_buf()],
        format,
        ..CheckArgs::default()
    }
}

#[test]
fn broken_file_gets_mlc000_and_other_files_are_still_analyzed() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path());
    let report = check(&args_for(project.path(), OutputFormat::Concise)).unwrap();

    let mlc000: Vec<_> = report
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.rule_id == "MLC000")
        .collect();
    assert_eq!(mlc000.len(), 1, "exactly one syntax error");
    let broken = mlc000[0];
    assert_eq!(broken.path, "scenes/bad.py");
    assert_eq!(broken.severity, Severity::Error);
    assert_eq!(broken.primary_span.start.line, 2);
    // 13 characters precede `def`, so the Unicode character column is 14
    // even though the UTF-8 byte column is larger.
    assert_eq!(broken.primary_span.start.column, 14);

    // The valid file was analyzed after the broken one failed: its unknown
    // suppression ID produced MLC001.
    let mlc001: Vec<_> = report
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.rule_id == "MLC001")
        .collect();
    assert_eq!(mlc001.len(), 1);
    assert_eq!(mlc001[0].path, "scenes/good.py");
    assert!(mlc001[0].message.contains("MLC999"));

    // Default excludes keep .venv out of the analysis.
    assert!(
        report
            .diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.path.starts_with(".venv")),
        ".venv must be excluded"
    );

    // MLC000 error reaches fail-level warning.
    assert_eq!(report.exit, ExitStatus::Failure);
}

#[test]
fn json_output_has_all_schema_required_keys() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path());
    let report = check(&args_for(project.path(), OutputFormat::Json)).unwrap();

    let value: serde_json::Value = serde_json::from_str(&report.output).unwrap();
    assert_eq!(value["schema_version"], 1);
    for key in ["tool_version", "project_root", "profiles", "diagnostics"] {
        assert!(value.get(key).is_some(), "missing envelope key {key}");
    }
    assert_eq!(value["profiles"], serde_json::json!(["production"]));

    let diagnostics = value["diagnostics"].as_array().unwrap();
    assert!(!diagnostics.is_empty());
    for diagnostic in diagnostics {
        for key in [
            "rule_id",
            "severity",
            "confidence",
            "path",
            "span",
            "message",
            "applicable_profiles",
            "evidence",
            "fix",
        ] {
            assert!(
                diagnostic.get(key).is_some(),
                "missing required diagnostic key {key}"
            );
        }
        assert!(diagnostic["fix"].is_null(), "phase 0 has no fixes");
        assert!(diagnostic["evidence"].is_object());
        assert!(diagnostic["applicable_profiles"].is_array());
        let span = &diagnostic["span"];
        for position in ["start", "end"] {
            assert!(span[position]["line"].as_u64().unwrap() >= 1);
            assert!(span[position]["column"].as_u64().unwrap() >= 1);
        }
        let path = diagnostic["path"].as_str().unwrap();
        assert!(!path.contains('\\'), "paths must be POSIX");
        assert!(!Path::new(path).is_absolute(), "paths must be relative");
    }
}

#[test]
fn output_is_byte_identical_across_runs() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path());
    for format in [
        OutputFormat::Concise,
        OutputFormat::Full,
        OutputFormat::Json,
    ] {
        let first = check(&args_for(project.path(), format)).unwrap();
        let second = check(&args_for(project.path(), format)).unwrap();
        assert_eq!(first.output, second.output, "unstable output for {format}");
        assert_eq!(first.output.as_bytes(), second.output.as_bytes());
    }
}

#[test]
fn diagnostics_are_stably_sorted() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path());
    let report = check(&args_for(project.path(), OutputFormat::Concise)).unwrap();
    let mut sorted = report.diagnostics.clone();
    sorted.sort_by(manim_lint::diagnostic::Diagnostic::compare_stable);
    assert_eq!(report.diagnostics, sorted);
}

#[test]
fn exit_code_semantics_follow_thresholds() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path());

    // Nothing selected from the MLP category: exit 0.
    let mut args = args_for(project.path(), OutputFormat::Concise);
    args.select = vec!["MLP".to_owned()];
    let report = check(&args).unwrap();
    assert!(report.diagnostics.is_empty());
    assert_eq!(report.exit, ExitStatus::Success);

    // Only the MLC001 warning remains and fail-level=error: shown, exit 0.
    let mut args = args_for(project.path(), OutputFormat::Concise);
    args.ignore = vec!["MLC000".to_owned()];
    args.fail_level = Some(Severity::Error);
    let report = check(&args).unwrap();
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(report.diagnostics[0].rule_id, "MLC001");
    assert_eq!(report.exit, ExitStatus::Success);

    // Same warning with fail-level=info: exit 1.
    let mut args = args_for(project.path(), OutputFormat::Concise);
    args.ignore = vec!["MLC000".to_owned()];
    args.fail_level = Some(Severity::Info);
    let report = check(&args).unwrap();
    assert_eq!(report.exit, ExitStatus::Failure);
}

#[test]
fn valid_inline_suppression_suppresses_by_line() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path());
    let report = check(&args_for(project.path(), OutputFormat::Concise)).unwrap();
    // The `ignore[MLC108]` on good.py line 4 is valid: no MLC001 for it and
    // nothing else is reported on that line.
    assert!(
        report
            .diagnostics
            .iter()
            .all(|diagnostic| !(diagnostic.path == "scenes/good.py"
                && diagnostic.primary_span.start.line == 4))
    );
}

#[test]
fn cli_fps_override_beats_profile_value() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path());

    let report = check(&args_for(project.path(), OutputFormat::Concise)).unwrap();
    assert!((report.config.active_profiles[0].frame_rate - 30.0).abs() < f64::EPSILON);

    let mut args = args_for(project.path(), OutputFormat::Concise);
    args.fps = Some(24.0);
    let report = check(&args).unwrap();
    assert!((report.config.active_profiles[0].frame_rate - 24.0).abs() < f64::EPSILON);
}

#[test]
fn unknown_selector_on_cli_is_a_config_error() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path());
    let mut args = args_for(project.path(), OutputFormat::Concise);
    args.select = vec!["MLX123".to_owned()];
    assert!(check(&args).is_err(), "unknown selector must be exit 2");
}

#[test]
fn cost_command_reports_a_scene_breakdown() {
    // Phase 3 delivered `manim-lint cost`; detailed golden coverage lives
    // in `tests/cost_command.rs`.
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("demo.py"),
        "from manim import *\n\n\nclass Demo(Scene):\n    def construct(self):\n        self.wait(2)\n",
    )
    .unwrap();
    let execution = application::execute(Command::Cost {
        path: project.path().to_path_buf(),
        scene: None,
    })
    .expect("cost runs in Phase 3");
    assert!(execution.stdout.contains("scene demo.Demo"));
    assert!(execution.stdout.contains("wait duration 2 s"));
}

#[test]
fn explain_unknown_rule_is_an_error_and_known_rules_print_docs() {
    assert!(
        application::execute(Command::Explain {
            rule: "MLX999".to_owned()
        })
        .is_err()
    );

    let execution = application::execute(Command::Explain {
        rule: "MLC000".to_owned(),
    })
    .unwrap();
    assert!(execution.stdout.contains("MLC000"));
    assert_eq!(execution.exit, ExitStatus::Success);

    // Reserved rules are presented as reserved, never as checked.
    let execution = application::execute(Command::Explain {
        rule: "MLC114".to_owned(),
    })
    .unwrap();
    assert!(execution.stdout.contains("reserved"));
}

#[test]
fn rules_command_lists_reserved_ids_without_claiming_them() {
    let execution = application::execute(Command::Rules).unwrap();
    assert!(execution.stdout.contains("MLC000  phase 0  implemented"));
    assert!(execution.stdout.contains("MLC101  phase 1  implemented"));
    assert!(execution.stdout.contains("MLC107  phase 2  implemented"));
    assert!(execution.stdout.contains("MLC114  phase 2  reserved"));
    assert!(execution.stdout.contains("MLP225  phase 3  reserved"));
    assert!(execution.stdout.contains("MLD307  phase 4  implemented"));
    assert!(execution.stdout.contains("MLR118  phase 4  reserved"));
    // Every catalog family appears.
    for prefix in ["MLC", "MLR", "MLP", "MLD"] {
        assert!(execution.stdout.contains(prefix));
    }
}

#[test]
fn smoke_fixture_project_checks_end_to_end() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/smoke");
    let report = check(&args_for(&fixture, OutputFormat::Concise)).unwrap();
    assert_eq!(report.exit, ExitStatus::Failure, "broken.py has MLC000");
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.rule_id == "MLC000"
                && diagnostic.path == "scenes/broken.py")
    );
    // The fixture pyproject selects the "preview" profile.
    assert_eq!(report.config.default_profile, "preview");
    assert_eq!(report.config.active_profiles[0].pixel_width, 854);
}
