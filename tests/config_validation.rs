//! Configuration honesty tests (DESIGN §8.2, review follow-up): a setting
//! manim-lint accepts must actually be consulted, and a setting it cannot
//! honor must be an explicit configuration error (exit 2) instead of a
//! silently ignored value.

use std::path::Path;

use manim_lint::application::{ApplicationError, check, run_config_at};
use manim_lint::cli::{CheckArgs, Resolution};
use manim_lint::reporting::OutputFormat;

const SCENE: &str = "\
from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        self.play(FadeIn(square), run_time=0.004)
";

fn write_project(pyproject: &str) -> tempfile::TempDir {
    write_project_with(pyproject, SCENE)
}

fn write_project_with(pyproject: &str, scene: &str) -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("pyproject.toml"), pyproject).unwrap();
    std::fs::write(project.path().join("scene.py"), scene).unwrap();
    project
}

fn args_for(root: &Path) -> CheckArgs {
    CheckArgs {
        paths: vec![root.to_path_buf()],
        format: Some(OutputFormat::Concise),
        ..CheckArgs::default()
    }
}

fn expect_config_error(result: Result<manim_lint::application::CheckReport, ApplicationError>) {
    match result {
        Err(ApplicationError::Config(_)) => {}
        Err(other) => panic!("expected a config error (exit 2), got: {other}"),
        Ok(_) => panic!("expected a config error (exit 2), but the check ran"),
    }
}

#[test]
fn cli_fps_zero_is_a_config_error() {
    let project = write_project("[tool.manim-lint]\n");
    let mut args = args_for(project.path());
    args.fps = Some(0.0);
    expect_config_error(check(&args));
}

#[test]
fn cli_fps_non_finite_is_a_config_error() {
    let project = write_project("[tool.manim-lint]\n");
    for bad in [f64::NAN, f64::INFINITY, -1.0] {
        let mut args = args_for(project.path());
        args.fps = Some(bad);
        expect_config_error(check(&args));
    }
}

#[test]
fn cli_resolution_zero_is_a_config_error() {
    let project = write_project("[tool.manim-lint]\n");
    let mut args = args_for(project.path());
    args.resolution = Some("0x0".parse::<Resolution>().unwrap());
    expect_config_error(check(&args));
}

#[test]
fn profile_zero_frame_rate_is_a_config_error() {
    let project = write_project(
        "[tool.manim-lint]\ndefault-profile = \"p\"\n\n\
         [[tool.manim-lint.profile]]\nname = \"p\"\nframe-rate = 0\n",
    );
    expect_config_error(check(&args_for(project.path())));
}

#[test]
fn manim_cfg_zero_frame_rate_is_a_config_error() {
    let project = write_project("[tool.manim-lint]\n");
    std::fs::write(project.path().join("manim.cfg"), "[CLI]\nframe_rate = 0\n").unwrap();
    expect_config_error(check(&args_for(project.path())));
}

#[test]
fn manim_cfg_zero_pixel_width_is_a_config_error() {
    let project = write_project("[tool.manim-lint]\n");
    std::fs::write(project.path().join("manim.cfg"), "[CLI]\npixel_width = 0\n").unwrap();
    expect_config_error(check(&args_for(project.path())));
}

#[test]
fn stub_paths_is_an_honest_refusal() {
    let project = write_project("[tool.manim-lint]\nstub-paths = [\"stubs\"]\n");
    match check(&args_for(project.path())) {
        Err(ApplicationError::Config(error)) => {
            assert!(
                error
                    .to_string()
                    .contains("stub-paths is not implemented yet"),
                "refusal must name the unimplemented feature: {error}"
            );
        }
        other => panic!("expected a config error, got: {other:?}"),
    }
}

#[test]
fn manim_version_outside_knowledge_profile_range_is_a_config_error() {
    let project = write_project("[tool.manim-lint]\nmanim-version = \"0.19\"\n");
    match check(&args_for(project.path())) {
        Err(ApplicationError::Config(error)) => {
            let message = error.to_string();
            assert!(message.contains("upstream_0_20"), "{message}");
            assert!(message.contains(">=0.20,<0.21"), "{message}");
        }
        other => panic!("expected a config error, got: {other:?}"),
    }
}

#[test]
fn manim_version_inside_range_or_absent_is_accepted() {
    for pyproject in [
        "[tool.manim-lint]\nmanim-version = \"0.20\"\n",
        "[tool.manim-lint]\nmanim-version = \"0.20.1\"\n",
        "[tool.manim-lint]\n",
    ] {
        let project = write_project(pyproject);
        assert!(
            check(&args_for(project.path())).is_ok(),
            "must be accepted: {pyproject}"
        );
    }
}

/// The shipped knowledge profile's range must stay parseable by the
/// validator, so the manim-version check can never silently degrade to
/// "informational" for the default profile.
#[test]
fn shipped_knowledge_profile_range_is_enforceable() {
    let profile = manim_lint::knowledge::load(manim_lint::knowledge::DEFAULT_PROFILE).unwrap();
    // In-range passes, out-of-range fails: both directions prove the
    // range parsed instead of being skipped.
    assert!(
        manim_lint::config::loader::validate_manim_version(
            "0.20",
            &profile.name,
            &profile.manim_version
        )
        .is_ok()
    );
    assert!(
        manim_lint::config::loader::validate_manim_version(
            "0.19",
            &profile.name,
            &profile.manim_version
        )
        .is_err()
    );
}

#[test]
fn target_python_newer_than_parser_grammar_is_a_config_error() {
    for bad in ["3.13", "4.0", "2.7", "3.11.2", "py3"] {
        let project = write_project(&format!("[tool.manim-lint]\ntarget-python = \"{bad}\"\n"));
        expect_config_error(check(&args_for(project.path())));
    }
}

/// Targets below the syntax-gating floor (3.6) are refused with a message
/// naming the floor: the gate cannot guarantee older parsers, so accepting
/// the target would be a silently unenforced promise.
#[test]
fn target_python_below_the_gating_floor_is_a_config_error() {
    for bad in ["3.0", "3.5"] {
        let project = write_project(&format!("[tool.manim-lint]\ntarget-python = \"{bad}\"\n"));
        match check(&args_for(project.path())) {
            Err(ApplicationError::Config(error)) => {
                let message = error.to_string();
                assert!(
                    message.contains("cannot guarantee syntax gating")
                        && message.contains("minimum enforceable target-python is 3.6"),
                    "floor error must name the floor: {message}"
                );
            }
            other => panic!("target-python {bad} must be a config error, got: {other:?}"),
        }
    }
}

#[test]
fn target_python_in_supported_range_is_accepted() {
    for good in ["3.6", "3.8", "3.11", "3.12"] {
        let project = write_project(&format!("[tool.manim-lint]\ntarget-python = \"{good}\"\n"));
        assert!(
            check(&args_for(project.path())).is_ok(),
            "target-python {good} must be accepted"
        );
    }
}

/// DESIGN §5.2 adapted: the parser cannot pin `feature_version`, so a
/// construct newer than `target-python` is caught by the post-parse
/// syntax gate as MLC000 — and the file is still analyzed (the gate never
/// removes the AST).
#[test]
fn target_python_gates_match_statements() {
    const MATCH_SCENE: &str = "\
from manim import *


class Chooser(Scene):
    def construct(self):
        command = 1
        match command:
            case 1:
                self.play(FadeIn(Square()), run_time=0.004)
";
    let project = write_project_with("[tool.manim-lint]\ntarget-python = \"3.9\"\n", MATCH_SCENE);
    let report = check(&args_for(project.path())).unwrap();
    let gate: Vec<_> = report
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.rule_id == "MLC000")
        .collect();
    assert_eq!(gate.len(), 1, "exactly one gated construct");
    assert_eq!(
        gate[0].message,
        "`match` statement requires Python 3.10 but target-python is 3.9"
    );
    assert_eq!(gate[0].primary_span.start.line, 7);
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.rule_id == "MLP206"),
        "analysis of the gated file continues: the sub-frame play inside \
         the match arm still reports"
    );

    let project = write_project_with("[tool.manim-lint]\ntarget-python = \"3.10\"\n", MATCH_SCENE);
    let report = check(&args_for(project.path())).unwrap();
    assert!(
        report
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.rule_id != "MLC000"),
        "the same file is clean once the target reaches 3.10"
    );
}

#[test]
fn target_python_gates_type_alias_statements() {
    const TYPE_SCENE: &str = "type Vector = list[float]\n";
    let project = write_project_with("[tool.manim-lint]\ntarget-python = \"3.11\"\n", TYPE_SCENE);
    let report = check(&args_for(project.path())).unwrap();
    assert!(
        report.diagnostics.iter().any(|diagnostic| {
            diagnostic.rule_id == "MLC000"
                && diagnostic.message
                    == "`type` alias statement requires Python 3.12 but target-python is 3.11"
        }),
        "diagnostics: {:?}",
        report.diagnostics
    );
    let project = write_project_with("[tool.manim-lint]\ntarget-python = \"3.12\"\n", TYPE_SCENE);
    let report = check(&args_for(project.path())).unwrap();
    assert!(report.diagnostics.is_empty());
}

#[test]
fn target_python_gates_walrus_below_3_8() {
    const WALRUS_SCENE: &str = "value = 1\nif (flag := value) > 0:\n    pass\n";
    let project = write_project_with("[tool.manim-lint]\ntarget-python = \"3.7\"\n", WALRUS_SCENE);
    let report = check(&args_for(project.path())).unwrap();
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.rule_id == "MLC000"
            && diagnostic.message
                == "assignment expression `:=` requires Python 3.8 but target-python is 3.7"
    }));
    let project = write_project_with("[tool.manim-lint]\ntarget-python = \"3.8\"\n", WALRUS_SCENE);
    let report = check(&args_for(project.path())).unwrap();
    assert!(report.diagnostics.is_empty(), "walrus is fine at 3.8");
}

/// Reverse direction of the syntax gate: `async` / `await` were soft
/// keywords until 3.7, so `async = 1` is valid Python 3.6 source that the
/// bundled 3.12 grammar cannot parse. Under a 3.6 target the parse-failure
/// MLC000 carries a hedged hint; under 3.7+ (where the source really is a
/// syntax error) it does not.
#[test]
fn pre37_async_identifier_parse_failure_carries_the_hint_under_3_6() {
    const HINT: &str = "this may be valid Python 3.6 source";
    let project = write_project_with(
        "[tool.manim-lint]\ntarget-python = \"3.6\"\n",
        "async = 1\n",
    );
    let report = check(&args_for(project.path())).unwrap();
    let diagnostic = report
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.rule_id == "MLC000")
        .expect("the file fails to parse under the 3.12 grammar");
    assert!(
        diagnostic.message.contains(HINT) && diagnostic.message.contains("3.12 grammar"),
        "the 3.6 target must hint at the grammar mismatch: {}",
        diagnostic.message
    );

    for target in ["3.7", "3.12"] {
        let project = write_project_with(
            &format!("[tool.manim-lint]\ntarget-python = \"{target}\"\n"),
            "async = 1\n",
        );
        let report = check(&args_for(project.path())).unwrap();
        let diagnostic = report
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.rule_id == "MLC000")
            .expect("still a parse failure");
        assert!(
            !diagnostic.message.contains(HINT),
            "targets at or above 3.7 reserve the keywords for real: {}",
            diagnostic.message
        );
    }
}

/// The hint stays conservative: a 3.6-target parse failure with no
/// `async` / `await` near the error is reported without the note.
#[test]
fn pre37_hint_is_absent_when_the_failure_does_not_mention_async() {
    let project = write_project_with(
        "[tool.manim-lint]\ntarget-python = \"3.6\"\n",
        "def broken(:\n",
    );
    let report = check(&args_for(project.path())).unwrap();
    let diagnostic = report
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.rule_id == "MLC000")
        .expect("parse failure");
    assert!(
        !diagnostic.message.contains("Python 3.6 source"),
        "an unrelated syntax error must not gain the hint: {}",
        diagnostic.message
    );
}

#[test]
fn config_command_states_what_is_enforced() {
    let project = write_project("[tool.manim-lint]\nmanim-version = \"0.20\"\n");
    let execution = run_config_at(project.path()).unwrap();
    let value: serde_json::Value = serde_json::from_str(&execution.stdout).unwrap();
    let enforcement = value
        .get("enforcement")
        .expect("config output carries an enforcement section");
    let target_python = enforcement["target-python"].as_str().unwrap();
    assert!(
        target_python.contains("does not change parsing"),
        "target-python note must state the grammar is fixed: {target_python}"
    );
    assert!(
        target_python.contains("no feature_version pinning"),
        "{target_python}"
    );
    assert!(
        target_python.contains("MLC000"),
        "the note must state the post-parse gate: {target_python}"
    );
    let manim_version = enforcement["manim-version"].as_str().unwrap();
    assert!(manim_version.contains("upstream_0_20"), "{manim_version}");
    assert!(manim_version.contains(">=0.20,<0.21"), "{manim_version}");
    let stub_paths = enforcement["stub-paths"].as_str().unwrap();
    assert!(stub_paths.contains("not implemented"), "{stub_paths}");
}

#[test]
fn config_command_reports_config_errors_with_exit_2_semantics() {
    let project = write_project("[tool.manim-lint]\nmanim-version = \"0.19\"\n");
    assert!(
        matches!(
            run_config_at(project.path()),
            Err(ApplicationError::Config(_))
        ),
        "config must fail on the same validation errors as check"
    );
}

/// Regression for the MLP206 direction fix: a sub-frame play renders a
/// single frame at t=0 (`np.arange(0, run_time, 1/fps)`), i.e. near the
/// START state; `finish()` mutates geometry afterwards but writes no
/// extra frame. The old text claimed the opposite ("shows only its final
/// state").
#[test]
fn mlp206_explanation_states_the_start_state_direction() {
    let project = write_project("[tool.manim-lint]\n");
    let report = check(&args_for(project.path())).unwrap();
    let diagnostic = report
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.rule_id == "MLP206")
        .expect("the 0.004 s play fires MLP206 at the default 60 fps");
    let explanation = diagnostic.explanation.as_deref().unwrap_or_default();
    assert!(
        explanation.contains("near the start state"),
        "explanation must state the frame samples the start state: {explanation}"
    );
    assert!(
        !explanation.contains("shows only its final state"),
        "the wrong direction must be gone: {explanation}"
    );
    assert!(
        explanation.contains("writes no extra frame"),
        "finish() semantics must be stated: {explanation}"
    );
}
