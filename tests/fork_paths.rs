//! Fork-aware cost report and `MLP225` tests (DESIGN §7.3).
//!
//! The `MLP225` prose is binding: not a normal warning
//! (`default_enabled: false`, capabilities `cost-report` +
//! `local-fork-overlay`), causal explanations only ("fork-per-play is
//! serial fallback because of this Scene updater"), never removal
//! advice, fork loss only when the active profile requests
//! `cairo_fork_workers >= 2`, and the monotonic renderer-wide disable
//! modeled — per-play independence is never assumed. Everything is
//! inert under `upstream_0_20`: the cost-report section is absent and
//! the rule reports nothing even when explicitly selected.

use std::path::Path;

use manim_lint::application::{check, run_cost};
use manim_lint::cli::CheckArgs;
use manim_lint::diagnostic::{Confidence, Severity};
use manim_lint::reporting::OutputFormat;
use manim_lint::rules::registry;

/// The DESIGN §8.2 fork-profile configuration: the local overlay plus a
/// production profile requesting the fork pipeline and static layers.
const FORK_PYPROJECT: &str = "\
[tool.manim-lint]
knowledge-profile = \"local_0_20_1_4d25c031\"
default-profile = \"production\"

[[tool.manim-lint.profile]]
name = \"production\"
renderer = \"cairo\"
platform = \"linux\"
cairo-fork-workers = 4
cairo-static-layers = true
";

/// Same overlay, but the fork pipeline is unrequested (`workers 0` is
/// not a blocker and never a reported loss — DESIGN §7.3).
const UNREQUESTED_PYPROJECT: &str = "\
[tool.manim-lint]
knowledge-profile = \"local_0_20_1_4d25c031\"
default-profile = \"production\"

[[tool.manim-lint.profile]]
name = \"production\"
renderer = \"cairo\"
platform = \"linux\"
cairo-fork-workers = 0
cairo-static-layers = true
";

/// A Scene updater blocks the fork from play #2; it is provably removed
/// before play #3, so play #3 carries no blocker of its own — the
/// monotonic renderer-wide disable is the only thing keeping it serial.
const MONOTONIC_SCENE: &str = "\
from manim import *


def pause_updates(dt):
    pass


class Fork(Scene):
    def construct(self):
        square = Square()
        self.add(square)
        self.play(FadeIn(square), run_time=2)
        self.add_updater(pause_updates)
        self.play(square.animate.shift(RIGHT), run_time=2)
        self.remove_updater(pause_updates)
        self.play(FadeOut(square), run_time=2)
";

fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, text) in files {
        std::fs::write(dir.path().join(name), text).unwrap();
    }
    dir
}

fn cost_output(root: &Path) -> String {
    run_cost(root, None).unwrap().stdout
}

fn check_args(root: &Path, select: &[&str]) -> CheckArgs {
    CheckArgs {
        paths: vec![root.to_path_buf()],
        format: Some(OutputFormat::Concise),
        select: select
            .iter()
            .map(|selector| (*selector).to_owned())
            .collect(),
        ..CheckArgs::default()
    }
}

/// The binding metadata contract (DESIGN §7.3 `MLP225` prose).
#[test]
fn mlp225_metadata_matches_the_binding_design_prose() {
    let metadata = registry::metadata_for("MLP225").expect("MLP225 is implemented");
    assert!(!metadata.default_enabled, "MLP225 is opt-in only");
    assert_eq!(metadata.default_severity, Severity::Info);
    assert_eq!(metadata.minimum_confidence, Confidence::High);
    assert_eq!(metadata.implementation_phase, 3);
    assert_eq!(
        metadata.required_capabilities,
        ["cost-report", "local-fork-overlay"],
        "the two required capabilities are prescribed by DESIGN §7.3"
    );
}

/// The fork fast-path section under the fork profile: play #1 clear,
/// play #2 serial fallback with the Scene-updater cause span, play #3
/// eligible-but-disabled with the causal chain (monotonic renderer-wide
/// disable — never per-play independence), plus the measured evidence
/// citation and the no-removal-advice note.
#[test]
fn cost_report_explains_the_monotonic_fork_disable_causally() {
    let dir = project(&[
        ("pyproject.toml", FORK_PYPROJECT),
        ("demo.py", MONOTONIC_SCENE),
    ]);
    let output = cost_output(dir.path());

    let expected_section = "\
  fork fast paths (profile production, knowledge local_0_20_1_4d25c031):
    fork-per-play (cairo_fork_workers 4):
      demo.py:12:9 play #1: no static blocker found (fork-eligible pending the runtime audit)
      demo.py:14:9 play #2: serial fallback because a Scene updater is registered at demo.py:13:9 (blocker scene_updaters)
      demo.py:16:9 play #3: no static blocker found, but play #2 (demo.py:14:9) fell back to the serial path (scene_updaters) and a rendered serial play opens the parent encoder: fork disabling is renderer-wide and monotonic (blocker parent_encoder_opened)
      evidence: measured fork-per-play A/B on the calibration machine at 1080p: Bayes 7.55 -> 3.95 s, Algorithm 12.13 -> 7.50 s (docs/research/perf-evidence.md)
    static layers (cairo_static_layers on):
      demo.py:12:9 play #1: no static blocker found
      demo.py:14:9 play #2: legacy static path because a Scene updater is registered at demo.py:13:9 (blocker scene_updaters)
      demo.py:16:9 play #3: no static blocker found
    packed interpolation:
      demo.py:12:9 play #1: canonical per-member interpolation because the animation type FadeIn is outside the audited allowlist at demo.py:12:19 (blocker unsupported_animation_type)
      demo.py:14:9 play #2: canonical per-member interpolation because a Scene updater is registered at demo.py:13:9 (blocker scene_updaters)
      demo.py:16:9 play #3: canonical per-member interpolation because the animation type FadeOut is outside the audited allowlist at demo.py:16:19 (blocker unsupported_animation_type)
      evidence: measured packed interpolation on the calibration machine, 300 members / 60 frames: 130.658 -> 33.004 ms/play, steady state 2.0761 -> 0.1890 ms/frame (docs/research/perf-evidence.md)
    note: the features named above can be correct expression; this section explains the render-path consequence and never advises removing them
";
    assert!(
        output.contains(expected_section),
        "the fork fast-path section must match exactly:\n{output}"
    );
    // Causality only — no removal advice anywhere.
    assert!(
        !output.contains("remove the"),
        "no removal advice: {output}"
    );
    assert!(!output.contains("delete"), "no removal advice: {output}");
}

/// Workers 0 is unrequested, never a blocker and never a reported fork
/// loss (DESIGN §7.3). The independently requested static-layer gate is
/// still evaluated.
#[test]
fn workers_zero_is_unrequested_and_reports_no_fork_loss() {
    let dir = project(&[
        ("pyproject.toml", UNREQUESTED_PYPROJECT),
        ("demo.py", MONOTONIC_SCENE),
    ]);
    let output = cost_output(dir.path());

    assert!(
        output.contains(
            "fork-per-play: not requested (cairo_fork_workers 0 is below the \
             enabling minimum of 2); nothing to report"
        ),
        "workers 0 must read as unrequested: {output}"
    );
    assert!(
        !output.contains("serial fallback"),
        "an unrequested pipeline has no fork loss to report: {output}"
    );
    assert!(
        !output.contains("parent encoder"),
        "no monotonic chain without a requested pipeline: {output}"
    );
    // cairo_static_layers = true is its own request and still evaluates.
    assert!(
        output.contains("legacy static path because a Scene updater is registered"),
        "the static-layer gate is independent of the fork workers: {output}"
    );
}

/// Under the upstream profile the section is absent entirely — no fork
/// vocabulary, no gates, no losses (DESIGN §7.3 inertness).
#[test]
fn upstream_profile_has_no_fork_paths_section() {
    let dir = project(&[("demo.py", MONOTONIC_SCENE)]);
    let output = cost_output(dir.path());
    assert!(
        !output.contains("fork fast paths"),
        "no fork section under upstream_0_20: {output}"
    );
    assert!(!output.contains("fork-per-play"), "{output}");
    assert!(!output.contains("cairo_fork_workers"), "{output}");
}

/// A normal `check` run never fires MLP225 — not under the builtin
/// select and not under the whole `MLP` family prefix.
#[test]
fn check_never_fires_mlp225_by_default() {
    let dir = project(&[
        ("pyproject.toml", FORK_PYPROJECT),
        ("demo.py", MONOTONIC_SCENE),
    ]);
    let report = check(&check_args(dir.path(), &[])).unwrap();
    assert!(
        report
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.rule_id != "MLP225"),
        "default check must not fire the opt-in rule: {:?}",
        report.diagnostics
    );
    let report = check(&check_args(dir.path(), &["MLP"])).unwrap();
    assert!(
        report
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.rule_id != "MLP225"),
        "the family prefix must not enable the opt-in rule"
    );
}

/// `--select MLP225` is the explicit opt-in: under the fork profile the
/// rule emits the same causal explanations as the cost report,
/// including the monotonic chain for play #3, as `info` diagnostics
/// (exit stays success under the default warning fail-level).
#[test]
fn explicit_select_evaluates_mlp225_under_the_fork_profile() {
    let dir = project(&[
        ("pyproject.toml", FORK_PYPROJECT),
        ("demo.py", MONOTONIC_SCENE),
    ]);
    let report = check(&check_args(dir.path(), &["MLP225"])).unwrap();
    let messages: Vec<&str> = report
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.rule_id == "MLP225")
        .map(|diagnostic| diagnostic.message.as_str())
        .collect();
    assert!(
        messages.iter().any(
            |message| message.contains("serial fallback because a Scene updater is registered")
        ),
        "the play #2 loss must cite its cause: {messages:?}"
    );
    assert!(
        messages.iter().any(|message| {
            message.contains("play #3")
                && message.contains("fork disabling is renderer-wide and monotonic")
        }),
        "play #3 must carry the monotonic causal chain: {messages:?}"
    );
    for diagnostic in &report.diagnostics {
        assert_eq!(diagnostic.severity, Severity::Info);
        assert_eq!(diagnostic.confidence, Confidence::High);
        // Causal explanation, never removal advice.
        let explanation = diagnostic.explanation.as_deref().unwrap_or_default();
        assert!(explanation.contains("not a defect report"), "{explanation}");
        assert!(
            !diagnostic.message.contains("remove the"),
            "no removal advice"
        );
    }
    assert_eq!(report.exit, manim_lint::cli::ExitStatus::Success);
}

/// Even the explicit opt-in stays inert under `upstream_0_20`: the
/// upstream profile declares no fork capabilities, so the rule has
/// nothing to interpret (DESIGN §7.3).
#[test]
fn explicit_select_stays_inert_under_the_upstream_profile() {
    let dir = project(&[("demo.py", MONOTONIC_SCENE)]);
    let report = check(&check_args(dir.path(), &["MLP225"])).unwrap();
    assert!(
        report.diagnostics.is_empty(),
        "MLP225 must be inert under upstream_0_20: {:?}",
        report.diagnostics
    );
}

/// The rules command and explain now report MLP225 as implemented.
#[test]
fn mlp225_is_listed_as_implemented() {
    assert!(registry::is_implemented("MLP225"));
    assert!(registry::is_reserved_rule_id("MLP225"));
}
