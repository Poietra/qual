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
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("pyproject.toml"), pyproject).unwrap();
    std::fs::write(project.path().join("scene.py"), SCENE).unwrap();
    project
}

fn args_for(root: &Path) -> CheckArgs {
    CheckArgs {
        paths: vec![root.to_path_buf()],
        format: OutputFormat::Concise,
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

#[test]
fn target_python_in_supported_range_is_accepted() {
    for good in ["3.8", "3.11", "3.12"] {
        let project = write_project(&format!("[tool.manim-lint]\ntarget-python = \"{good}\"\n"));
        assert!(
            check(&args_for(project.path())).is_ok(),
            "target-python {good} must be accepted"
        );
    }
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
