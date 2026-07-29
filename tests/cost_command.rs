//! `qual cost` golden tests (DESIGN §8.1, §4.1, roadmap Phase 3).
//!
//! The report shows per-scene plays with frame estimates, hot contexts
//! with entry kind / provenance / multiplicity factors and a per-callback
//! liveness note, per-frame constructions, and resource-key growth.
//! Bounds appear only when literal durations prove them **and** the
//! callback provably executes during the counted plays (DESIGN §3.2
//! updater suspension, §3.3 wait dynamics): a suspended `always_redraw`
//! reports "no proven per-frame execution" instead of a fabricated
//! invocation count. Unknown quantities print as "unknown" / "per-frame"
//! and never as a number (DESIGN §15 invariant 9).

use std::path::Path;

use qual::application::{ApplicationError, run_cost};

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

/// The review's canonical false positive: `FadeIn(label)` suspends the
/// animated mobject's updaters for the whole play (`animation.py`
/// `begin` → `suspend_updating`), and the un-`dt`'d factory leaves the
/// default `wait(3)` a static frame (`scene.py`
/// `should_update_mobjects`), so the callback runs about once — never
/// ~120 times per frame.
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

/// A true positive with proven bounds: the play animates the unrelated
/// `square`, so `label`'s updater provably runs during its 2 s; the
/// one-argument updater leaves `wait(3)` static, so those frames are
/// honestly excluded.
const LIVE_SCENE: &str = "\
from manim import *


class Live(Scene):
    def construct(self):
        tracker = ValueTracker(0)
        square = Square()
        label = always_redraw(lambda: MathTex(f\"x = {tracker.get_value():.2f}\"))
        self.add(square, label)
        self.play(square.animate.shift(LEFT), run_time=2)
        self.wait(3)
";

const UNKNOWN_SCENE: &str = "\
from manim import *


class Loose(Scene):
    def construct(self):
        square = Square()
        square.add_updater(lambda m: m.become(Text(\"hot\")))
        anchor = Circle()
        self.add(square, anchor)
        self.play(FadeIn(anchor))
        self.wait()
";

#[test]
fn cost_report_shows_no_quantity_for_suspended_updaters() {
    let dir = project(&[("demo.py", LITERAL_SCENE)]);
    let output = cost_output(dir.path(), None).unwrap();

    // Stable golden output for the whole scene section (default profile:
    // cairo 1920x1080 at 60 fps). The play/wait rows keep their honest
    // video-frame estimates, but the callback has no proven per-frame
    // execution — no "~120 invocations" is ever fabricated for it.
    let expected = "\
profiles: default (cairo, 1920x1080, 60 fps)

scene demo.Demo (demo.py)
  plays:
    demo.py:9:9 play duration 2 s -> frames ~120
    demo.py:10:9 wait duration 3 s -> frames ~180
  hot contexts:
    demo.py:7:31 entry always_redraw; path construct -> always_redraw:7; factors frames; proven execution plays: none
  per-frame constructions:
    demo.py:7:39 MathTex construction: no proven per-frame execution
  resource-key growth:
    demo.py:7:39 MathTex distinct cache keys: no proven per-frame execution (f-string key varies per frame)
";
    assert_eq!(output, expected);
}

#[test]
fn cost_report_shows_real_bounds_for_proven_execution_plays() {
    let dir = project(&[("live.py", LIVE_SCENE)]);
    let output = cost_output(dir.path(), None).unwrap();

    // The updater provably executes during the 2 s play of the *other*
    // mobject (~120 frames); the static wait(3) is excluded from the
    // estimate even though its 180 video frames still render.
    let expected = "\
profiles: default (cairo, 1920x1080, 60 fps)

scene live.Live (live.py)
  plays:
    live.py:10:9 play duration 2 s -> frames ~120
    live.py:11:9 wait duration 3 s -> frames ~180
  hot contexts:
    live.py:8:31 entry always_redraw; path construct -> always_redraw:8; factors frames; proven execution plays: live.py:10:9
  per-frame constructions:
    live.py:8:39 MathTex construction x ~120 invocations across 1 proven play(s)
  resource-key growth:
    live.py:8:39 MathTex distinct cache keys: ~120 across 1 proven play(s) (f-string key varies per frame)
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
    // The callback provably executes during the play of the unrelated
    // anchor, but the play's duration is unknown: per-frame, no number.
    assert!(output.contains("Text construction x per-frame"));
    assert!(output.contains("proven execution plays: loose.py:10:9"));
    // No invented frame numbers appear anywhere in the play rows.
    for line in output.lines().filter(|line| line.contains("-> frames")) {
        assert!(
            line.ends_with("per-frame"),
            "unknown duration must not produce a number: {line}"
        );
    }
}

/// A play issued inside a `self.<helper>()` body: the cost report counts
/// it as a play row at the helper site, and the tracker label's
/// `always_redraw` callback proves execution during it (the helper-driven
/// play animates the unrelated square).
const HELPER_SCENE: &str = "\
from manim import *


class Helper(Scene):
    def flash(self, mob):
        self.play(FadeIn(mob), run_time=2)

    def construct(self):
        tracker = ValueTracker(0)
        square = Square()
        label = always_redraw(lambda: MathTex(f\"x = {tracker.get_value():.2f}\"))
        self.add(square, label)
        self.flash(square)
";

#[test]
fn cost_report_counts_helper_plays() {
    let dir = project(&[("helper.py", HELPER_SCENE)]);
    let output = cost_output(dir.path(), None).unwrap();
    assert!(
        output.contains("helper.py:6:9 play duration 2 s -> frames ~120"),
        "the helper-body play must appear as a play row: {output}"
    );
    assert!(
        output.contains("proven execution plays: helper.py:6:9"),
        "the callback's liveness must cite the helper play: {output}"
    );
    assert!(
        output.contains("MathTex construction x ~120 invocations across 1 proven play(s)"),
        "the frame bound must come from the helper play's literal run_time: {output}"
    );
}

/// One helper played from two call sites (`self.flash(a)` then
/// `self.flash(b)`): each call site renders its own 2 s frame grid, so
/// the report lists one play row per execution and the callback's
/// quantities sum both — 240 invocations across 2 proven plays, never a
/// collapsed 120 / 1.
const TWO_CALL_SITE_HELPER_SCENE: &str = "\
from manim import *


class Twice(Scene):
    def flash(self, mob):
        self.play(FadeIn(mob), run_time=2)

    def construct(self):
        tracker = ValueTracker(0)
        a = Square()
        b = Circle()
        label = always_redraw(lambda: MathTex(f\"x = {tracker.get_value():.2f}\"))
        self.add(a, b, label)
        self.flash(a)
        self.flash(b)
";

#[test]
fn cost_report_counts_each_helper_call_site_as_its_own_play() {
    let dir = project(&[("twice.py", TWO_CALL_SITE_HELPER_SCENE)]);
    let output = cost_output(dir.path(), None).unwrap();

    // One play row per execution of the helper play site.
    let play_rows = output
        .lines()
        .filter(|line| line.contains("twice.py:6:9 play duration 2 s -> frames ~120"))
        .count();
    assert_eq!(play_rows, 2, "one play row per helper call site: {output}");

    // Both executions prove the callback and both are counted: 2 × 120
    // frames, cited as 2 proven plays.
    assert!(
        output.contains("proven execution plays: twice.py:6:9, twice.py:6:9"),
        "the liveness note must cite both executions: {output}"
    );
    assert!(
        output.contains("MathTex construction x ~240 invocations across 2 proven play(s)"),
        "both call sites must be summed: {output}"
    );
    assert!(
        output.contains("MathTex distinct cache keys: ~240 across 2 proven play(s)"),
        "resource-key growth must count both executions: {output}"
    );
}

/// The fork fast-path section (DESIGN §7.3 MLP225) is local-fork-overlay
/// only: under the default `upstream_0_20` profile the cost report must
/// not mention fork paths at all — no gates, no `cairo_fork_workers`
/// semantics, no fork vocabulary (the deeper fork-profile golden tests
/// live in `tests/fork_paths.rs`).
#[test]
fn cost_report_has_no_fork_section_under_the_upstream_profile() {
    for fixture in [LITERAL_SCENE, LIVE_SCENE, UNKNOWN_SCENE] {
        let dir = project(&[("scene.py", fixture)]);
        let output = cost_output(dir.path(), None).unwrap();
        assert!(
            !output.contains("fork fast paths") && !output.contains("fork-per-play"),
            "fork-path interpretation must stay inert under upstream_0_20: {output}"
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

/// The `.animate`-on-a-helper-parameter repro: one helper play animating
/// the *parameter* via a builder, called with two different mobjects.
/// The play-level literal `run_time` decides every execution's duration
/// (scene.py `compile_animations` `setattr`s the kwarg onto each
/// animation), so both executions keep 2 s / ~120 frames — the duration
/// never widens to unknown because the call sites pass different
/// targets.
const ANIMATE_HELPER_SCENE: &str = "\
from manim import *


class Twice(Scene):
    def go(self, m):
        self.play(m.animate.shift(RIGHT), run_time=2)

    def construct(self):
        tracker = ValueTracker(0)
        a = Square()
        b = Circle()
        label = always_redraw(lambda: MathTex(f\"x = {tracker.get_value():.2f}\"))
        self.add(a, b, label)
        self.go(a)
        self.go(b)
";

#[test]
fn cost_report_keeps_animate_helper_durations_exact_per_execution() {
    let dir = project(&[("twice.py", ANIMATE_HELPER_SCENE)]);
    let output = cost_output(dir.path(), None).unwrap();

    let expected = "\
profiles: default (cairo, 1920x1080, 60 fps)

scene twice.Twice (twice.py)
  plays:
    twice.py:6:9 play duration 2 s -> frames ~120
    twice.py:6:9 play duration 2 s -> frames ~120
  hot contexts:
    twice.py:12:31 entry always_redraw; path construct -> always_redraw:12; factors frames; proven execution plays: twice.py:6:9, twice.py:6:9
  per-frame constructions:
    twice.py:12:39 MathTex construction x ~240 invocations across 2 proven play(s)
  resource-key growth:
    twice.py:12:39 MathTex distinct cache keys: ~240 across 2 proven play(s) (f-string key varies per frame)
";
    assert_eq!(output, expected);
}

/// A helper call site inside a literal `range(3)` loop plus a direct
/// call site: each execution renders its own 2 s frame grid, so the
/// looped site multiplies by its `[0, 3]` trip interval while the direct
/// site proves exactly ~120 frames — a composed `~120 to ~480`, never a
/// collapsed single execution.
const LOOPED_ANIMATE_HELPER_SCENE: &str = "\
from manim import *


class Looped(Scene):
    def go(self, m):
        self.play(m.animate.shift(RIGHT), run_time=2)

    def construct(self):
        tracker = ValueTracker(0)
        a = Square()
        b = Circle()
        label = always_redraw(lambda: MathTex(f\"x = {tracker.get_value():.2f}\"))
        self.add(a, b, label)
        for _ in range(3):
            self.go(a)
        self.go(b)
";

#[test]
fn cost_report_composes_looped_helper_call_site_frames() {
    let dir = project(&[("looped.py", LOOPED_ANIMATE_HELPER_SCENE)]);
    let output = cost_output(dir.path(), None).unwrap();

    // One play row per execution, each with the exact per-execution grid.
    let play_rows = output
        .lines()
        .filter(|line| line.contains("looped.py:6:9 play duration 2 s -> frames ~120"))
        .count();
    assert_eq!(play_rows, 2, "one play row per call site: {output}");

    // The direct call site proves ~120 frames; the looped site is
    // branch-dependent and adds up to 3 x 120 above.
    assert!(
        output.contains("MathTex construction x ~120 to ~480 invocations across 1 proven play(s)"),
        "loop trips must multiply the looped execution's frames: {output}"
    );
    assert!(
        output.contains("proven execution plays: looped.py:6:9"),
        "the direct execution must prove the callback: {output}"
    );
}

/// Wave-11 composition: an *imported* module-level helper (cross-file
/// inlining) inside a literal `range(3)` loop must multiply exactly like
/// a method helper — the caller's trip interval keys the helper play's
/// execution via the call path, and each execution keeps the literal 2 s
/// grid from the play-level `run_time` kwarg.
const IMPORTED_HELPER_LIB: &str = "\
from manim import *


def go(scene, m):
    scene.play(m.animate.shift(RIGHT), run_time=2)
";

const IMPORTED_LOOPED_SCENE: &str = "\
from helper_lib import go
from manim import *


class Looped(Scene):
    def construct(self):
        tracker = ValueTracker(0)
        a = Square()
        b = Circle()
        label = always_redraw(lambda: MathTex(f\"x = {tracker.get_value():.2f}\"))
        self.add(a, b, label)
        for _ in range(3):
            go(self, a)
        go(self, b)
";

#[test]
fn cost_report_composes_imported_helper_loop_frames() {
    let dir = project(&[
        ("helper_lib.py", IMPORTED_HELPER_LIB),
        ("looped.py", IMPORTED_LOOPED_SCENE),
    ]);
    let output = cost_output(dir.path(), None).unwrap();

    // One play row per call site, anchored in the helper's file, each
    // with the exact per-execution grid.
    let play_rows = output
        .lines()
        .filter(|line| line.contains("helper_lib.py:5:5 play duration 2 s -> frames ~120"))
        .count();
    assert_eq!(play_rows, 2, "one play row per call site: {output}");

    // The direct call site proves ~120 frames; the looped site is
    // branch-dependent and adds up to 3 x 120 above.
    assert!(
        output.contains("MathTex construction x ~120 to ~480 invocations across 1 proven play(s)"),
        "loop trips must multiply the looped execution's frames: {output}"
    );
}
