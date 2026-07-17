//! `manim-lint cost` golden tests (DESIGN §8.1, §4.1, roadmap Phase 3).
//!
//! The report shows per-scene plays with frame estimates, hot contexts
//! with entry kind / provenance / multiplicity factors, per-frame
//! constructions, and resource-key growth. Bounds appear only when literal
//! durations prove them; unknown quantities print as "unknown" /
//! "per-frame" and never as a number (DESIGN §15 invariant 9).

use std::path::Path;

use manim_lint::application::{ApplicationError, run_cost};

/// Writes fixture files into a fresh temp project.
fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, text) in files {
        std::fs::write(dir.path().join(name), text).unwrap();
    }
    dir
}

fn cost_output(root: &Path, scene: Option<&str>) -> Result<String, ApplicationError> {
    run_cost(root, scene).map(|execution| execution.stdout)
}

const LITERAL_SCENE: &str = "\
from manim import *


class Demo(Scene):
    def construct(self):
        tracker = ValueTracker(0)
        label = always_redraw(lambda: MathTex(f\"x = {tracker.get_value():.2f}\"))
        self.add(label)
        self.play(FadeIn(label), run_time=2)
        self.wait(3)
";

const UNKNOWN_SCENE: &str = "\
from manim import *


class Loose(Scene):
    def construct(self):
        square = Square()
        square.add_updater(lambda m: m.become(Text(\"hot\")))
        self.add(square)
        self.play(FadeIn(square))
        self.wait()
";

#[test]
fn cost_report_shows_real_bounds_for_literal_run_times() {
    let dir = project(&[("demo.py", LITERAL_SCENE)]);
    let output = cost_output(dir.path(), None).unwrap();

    // Stable golden output for the whole scene section (default profile:
    // cairo 1920x1080 at 60 fps).
    let expected = "\
profiles: default (cairo, 1920x1080, 60 fps)

scene demo.Demo (demo.py)
  plays:
    demo.py:9:9 play duration 2 s -> frames ~120
    demo.py:10:9 wait duration 3 s -> frames ~180
  hot contexts:
    demo.py:7:31 entry always_redraw; path construct -> always_redraw:7; factors frames
  per-frame constructions:
    demo.py:7:39 MathTex construction x ~120 invocations across literal plays
  resource-key growth:
    demo.py:7:39 MathTex distinct cache keys: ~120 across literal plays (f-string key varies per frame)
";
    assert_eq!(output, expected);
}

#[test]
fn cost_report_never_fabricates_numbers_for_unknown_durations() {
    let dir = project(&[("loose.py", UNKNOWN_SCENE)]);
    let output = cost_output(dir.path(), None).unwrap();

    // The play has no literal run_time and the wait uses the default
    // duration: both stay unquantified.
    assert!(output.contains("play duration unknown -> frames per-frame"));
    assert!(output.contains("wait duration unknown -> frames per-frame"));
    // The hot Text construction cannot be bound to a frame count either.
    assert!(output.contains("Text construction x per-frame"));
    // No invented frame numbers appear anywhere in the play rows.
    for line in output.lines().filter(|line| line.contains("-> frames")) {
        assert!(
            line.ends_with("per-frame"),
            "unknown duration must not produce a number: {line}"
        );
    }
}

#[test]
fn cost_scene_filter_selects_one_scene() {
    let dir = project(&[("demo.py", LITERAL_SCENE), ("loose.py", UNKNOWN_SCENE)]);

    let all = cost_output(dir.path(), None).unwrap();
    assert!(all.contains("scene demo.Demo"));
    assert!(all.contains("scene loose.Loose"));

    // Filter by bare class name.
    let filtered = cost_output(dir.path(), Some("Demo")).unwrap();
    assert!(filtered.contains("scene demo.Demo"));
    assert!(!filtered.contains("scene loose.Loose"));

    // Filter by qualified name.
    let qualified = cost_output(dir.path(), Some("loose.Loose")).unwrap();
    assert!(qualified.contains("scene loose.Loose"));
    assert!(!qualified.contains("scene demo.Demo"));
}

#[test]
fn cost_unknown_scene_name_is_a_cli_error() {
    let dir = project(&[("demo.py", LITERAL_SCENE)]);
    let error = cost_output(dir.path(), Some("Nope")).unwrap_err();
    // ApplicationError maps to exit code 2 in the CLI entry point.
    assert!(
        matches!(error, ApplicationError::Cli(ref message) if message.contains("unknown scene")),
        "unexpected error: {error}"
    );
}
