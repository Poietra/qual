//! Analysis-coverage reporting tests: the `coverage` subcommand golden
//! text and JSON shape, determinism, and the `--analysis-summary` flag's
//! guarantee that stdout and the exit code of `check` stay untouched.
//!
//! The fixture project deliberately contains one of each silence the
//! report must surface: an unresolved relative import, a star import
//! from a module the analyzer cannot enumerate, a call with an empty
//! candidate set, a resolved `manim.*` API absent from the knowledge
//! profile, a play with an unknown duration, and a construct above the
//! configured `target-python`.

use std::path::Path;

use qual::application::{run_check, run_coverage};
use qual::cli::CheckArgs;
use qual::reporting::coverage::CoverageFormat;

const PYPROJECT: &str = "[tool.qual]\ntarget-python = \"3.9\"\n";

const SCENE: &str = "\
from manim import *
from .missing import helper
from numpy import *
from manim.utils.rate_functions import ease_in_sine


class Demo(Scene):
    def construct(self):
        square = Square()
        rate = ease_in_sine(0.5)
        mystery(square)
        match rate:
            case _:
                pass
        self.play(FadeIn(square), run_time=2)
        self.play(FadeIn(square))
";

/// Writes fixture files into a fresh temp project.
fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pyproject.toml"), PYPROJECT).unwrap();
    for (name, text) in files {
        std::fs::write(dir.path().join(name), text).unwrap();
    }
    dir
}

fn coverage_output(root: &Path, format: CoverageFormat) -> String {
    let execution = run_coverage(&[root.to_path_buf()], format).unwrap();
    assert_eq!(execution.exit.code(), 0, "coverage always exits 0");
    assert!(execution.stderr.is_none(), "coverage writes stdout only");
    execution.stdout
}

#[test]
fn coverage_text_report_is_the_expected_golden() {
    let dir = project(&[("scene.py", SCENE)]);
    let output = coverage_output(dir.path(), CoverageFormat::Text);
    let expected = "\
analysis coverage (knowledge profile upstream_0_20, target-python 3.9)

scene.py
  constructs above target-python (MLC000): 1
  star imports from unresolved modules: 1
  unresolved relative imports: 1
  calls with no resolved target: 1 of 7 (mystery x1)
  manim APIs not in the knowledge profile: manim.utils.rate_functions.ease_in_sine

scene scene.Demo (scene.py)
  plays with unknown duration: 1 of 2
  .animate builders with unknown target: 0 of 0

project
  files parsed: 1 of 1
  calls resolved: 6 of 7
  play durations known: 1 of 2
  scene constructors resolved: 1 of 1
  constructs above target-python (MLC000): 1
  unresolved imports: 2 (1 star, 1 relative)
  manim APIs not in the knowledge profile: 1
  helper calls summarized, not inlined: 0
  top unresolved calls: mystery x1

analysis confidence: 1/1 files parsed, 6/7 calls resolved, 1/2 play durations \
known, 1/1 scene constructors resolved (counts of analyzed facts, not estimates)
";
    assert_eq!(output, expected);
}

#[test]
fn coverage_json_keys_are_stable_and_counts_match() {
    let dir = project(&[("scene.py", SCENE)]);
    let output = coverage_output(dir.path(), CoverageFormat::Json);
    let value: serde_json::Value = serde_json::from_str(&output).expect("valid JSON");

    for key in [
        "knowledge_profile",
        "target_python",
        "files",
        "scenes",
        "project",
    ] {
        assert!(value.get(key).is_some(), "missing top-level key {key}");
    }
    assert_eq!(value["knowledge_profile"], "upstream_0_20");
    assert_eq!(value["target_python"], "3.9");

    let file = &value["files"][0];
    for key in [
        "path",
        "parsed",
        "gated_constructs",
        "unresolved_star_imports",
        "unresolved_relative_imports",
        "calls",
        "unresolved_calls",
        "unresolved_call_names",
        "apis_not_in_profile",
    ] {
        assert!(file.get(key).is_some(), "missing file key {key}");
    }
    assert_eq!(file["path"], "scene.py");
    assert_eq!(file["parsed"], true);
    assert_eq!(file["gated_constructs"], 1);
    assert_eq!(file["unresolved_star_imports"], 1);
    assert_eq!(file["unresolved_relative_imports"], 1);
    assert_eq!(file["unresolved_calls"], 1);
    assert_eq!(file["unresolved_call_names"]["mystery"], 1);
    assert_eq!(
        file["apis_not_in_profile"],
        serde_json::json!(["manim.utils.rate_functions.ease_in_sine"])
    );

    let scene = &value["scenes"][0];
    for key in [
        "name",
        "path",
        "constructor_state_unknown",
        "plays",
        "plays_with_unknown_duration",
        "builders",
        "builders_with_unknown_target",
    ] {
        assert!(scene.get(key).is_some(), "missing scene key {key}");
    }
    assert_eq!(scene["name"], "scene.Demo");
    assert_eq!(scene["plays"], 2);
    assert_eq!(scene["plays_with_unknown_duration"], 1);
    // The helper-fallback frontier is recorded project-wide only
    // (deduplicated across scenes sharing a helper chain), so the
    // per-scene key stays absent — a split would fabricate attribution.
    assert!(scene.get("helper_inline_fallbacks").is_none());

    let project = &value["project"];
    for key in [
        "files",
        "files_parsed",
        "gated_constructs",
        "unresolved_star_imports",
        "unresolved_relative_imports",
        "calls",
        "unresolved_calls",
        "top_unresolved_call_names",
        "plays",
        "plays_with_unknown_duration",
        "scenes",
        "scenes_with_unknown_constructor_state",
        "builders",
        "builders_with_unknown_target",
        "apis_not_in_profile",
        "helper_inline_fallbacks",
    ] {
        assert!(project.get(key).is_some(), "missing project key {key}");
    }
    // The lifecycle fact layer exposes the inline-fallback frontier; this
    // fixture's helpers all inline, so the real count is zero.
    assert_eq!(project["helper_inline_fallbacks"], 0);
    assert_eq!(project["files_parsed"], 1);
    assert_eq!(project["unresolved_calls"], 1);
    assert_eq!(project["top_unresolved_call_names"][0]["name"], "mystery");
    assert_eq!(project["top_unresolved_call_names"][0]["count"], 1);
    assert_eq!(project["scenes_with_unknown_constructor_state"], 0);
}

#[test]
fn coverage_output_is_deterministic() {
    let dir = project(&[("scene.py", SCENE)]);
    for format in [CoverageFormat::Text, CoverageFormat::Json] {
        let first = coverage_output(dir.path(), format);
        let second = coverage_output(dir.path(), format);
        assert_eq!(first, second, "two runs must be byte-identical");
    }
}

#[test]
fn coverage_reports_unparsed_files() {
    let dir = project(&[("scene.py", SCENE), ("broken.py", "def broken(:\n")]);
    let output = coverage_output(dir.path(), CoverageFormat::Text);
    assert!(
        output.contains("broken.py\n  not parsed (decode or syntax failure; file skipped)"),
        "the unparsed file must be listed: {output}"
    );
    assert!(
        output.contains("files parsed: 1 of 2"),
        "project totals must count the unparsed file: {output}"
    );
}

#[test]
fn analysis_summary_never_changes_stdout_or_exit() {
    let dir = project(&[("scene.py", SCENE)]);
    let args = |summary: bool| CheckArgs {
        paths: vec![dir.path().to_path_buf()],
        analysis_summary: summary,
        ..CheckArgs::default()
    };

    let plain = run_check(&args(false)).unwrap();
    let with_summary = run_check(&args(true)).unwrap();

    assert_eq!(plain.stdout, with_summary.stdout, "stdout must not change");
    assert_eq!(plain.exit, with_summary.exit, "exit must not change");
    assert!(plain.stderr.is_none(), "no summary without the flag");
    let stderr = with_summary.stderr.expect("summary goes to stderr");
    assert!(
        stderr.contains("analysis coverage (knowledge profile upstream_0_20"),
        "stderr must carry the coverage section: {stderr}"
    );
    assert!(
        stderr.contains("plays with unknown duration: 1 of 2"),
        "the summary must include lifecycle counters even though no rule \
         selection forced the interpreter: {stderr}"
    );
}

/// Wave-11 composition fixture: a cross-file module-level helper that
/// fully inlines (`go`, reached from a loop and a direct call site) and
/// a self-recursive helper (`deepen`) whose cycle-closing call must fall
/// back to the effect summary.
const COMPOSED_HELPER_LIB: &str = "\
from manim import *


def go(scene, m):
    scene.play(m.animate.shift(RIGHT), run_time=2)


def deepen(scene, m):
    scene.play(FadeIn(m), run_time=1)
    deepen(scene, m)
";

const COMPOSED_SCENE: &str = "\
from helper_lib import go
from manim import *


class Composed(Scene):
    def construct(self):
        a = Square()
        b = Circle()
        for _ in range(3):
            go(self, a)
        go(self, b)
";

/// Scene-method recursion: the cycle-closing call falls back to the
/// method summary, whose play record rehydrates as a summary-derived
/// fact — the play behind the frontier stays counted.
const METHOD_RECURSION_SCENE: &str = "\
from manim import *


class MethodRecursion(Scene):
    def ping(self, m):
        self.play(FadeIn(m), run_time=1)
        self.ping(m)

    def construct(self):
        self.ping(Square())
";

/// Module-function recursion: summary runs are context-free and bind no
/// parameter as the scene, so `deepen`'s summary carries no play record
/// (documented scope limit in `interpreter/inline.rs`) — only the traced
/// first inline level keeps the site visible, and the frontier fact
/// records the loss.
const MODULE_RECURSION_SCENE: &str = "\
from helper_lib import deepen
from manim import *


class ModuleRecursion(Scene):
    def construct(self):
        deepen(self, Square())
";

/// Fully inlined cross-file helpers leave no frontier: the project count
/// is a real zero (the fact layer computed it), and every helper play is
/// counted with its literal duration known.
#[test]
fn coverage_reports_zero_fallbacks_when_helpers_fully_inline() {
    let dir = project(&[
        ("helper_lib.py", COMPOSED_HELPER_LIB),
        ("scene.py", COMPOSED_SCENE),
    ]);
    let output = coverage_output(dir.path(), CoverageFormat::Json);
    let value: serde_json::Value = serde_json::from_str(&output).expect("valid JSON");
    assert_eq!(value["project"]["helper_inline_fallbacks"], 0);
    let scene = &value["scenes"][0];
    assert_eq!(scene["name"], "scene.Composed");
    assert_eq!(scene["plays"], 2, "one traced play per call site");
    assert_eq!(scene["plays_with_unknown_duration"], 0);

    let text = coverage_output(dir.path(), CoverageFormat::Text);
    assert!(
        text.contains("helper calls summarized, not inlined: 0"),
        "the project section must show the real zero: {text}"
    );
}

/// Recursive helpers record their cycle-closing fallbacks on the
/// frontier while every play the interpreter still sees stays counted:
/// method recursion keeps both the traced and the summary-derived play;
/// module-function recursion keeps the traced first inline level (its
/// context-free summary carries no play record — the frontier fact is
/// what marks that loss).
#[test]
fn coverage_counts_summary_derived_plays_and_the_recursion_frontier() {
    let dir = project(&[
        ("helper_lib.py", COMPOSED_HELPER_LIB),
        ("method_rec.py", METHOD_RECURSION_SCENE),
        ("module_rec.py", MODULE_RECURSION_SCENE),
    ]);
    let output = coverage_output(dir.path(), CoverageFormat::Json);
    let value: serde_json::Value = serde_json::from_str(&output).expect("valid JSON");
    assert_eq!(
        value["project"]["helper_inline_fallbacks"], 2,
        "one cycle-closing fallback per recursive helper"
    );

    let method = &value["scenes"][0];
    assert_eq!(method["name"], "method_rec.MethodRecursion");
    assert_eq!(
        method["plays"], 2,
        "the traced play and the summary-derived play must both count"
    );
    assert_eq!(method["plays_with_unknown_duration"], 0);

    let module = &value["scenes"][1];
    assert_eq!(module["name"], "module_rec.ModuleRecursion");
    assert_eq!(
        module["plays"], 1,
        "the traced first inline level keeps the site visible"
    );
    assert_eq!(module["plays_with_unknown_duration"], 0);
}
