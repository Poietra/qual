//! Golden tests for the Phase 1 rendering rules (`MLR101`, `MLR103`,
//! `MLR104`, `MLR105`, `MLR106`, `MLR115`, `MLR117`, `MLR124`, `MLR126`)
//! and the Phase 2 state-dependent rules (`MLR102`, `MLR113`, `MLR114`,
//! `MLR125`, `MLR127`).
//!
//! Each rule has a fixture directory under `tests/fixtures/rules/<ID>/`
//! (DESIGN §11.1): `invalid.py` (true positives plus one inline-suppressed
//! case), `valid.py` (near-misses that must stay silent), and `alias.py`
//! (alias / module-alias import variants proving import style does not
//! change results; `invalid.py` itself uses the star-import style).
//! Phase 2 fixtures use `branch.py` (an Unknown / Maybe branch fact that
//! must silence the rule) and `suppressed.py` (the inline-suppression
//! case) instead of `alias.py`.
//!
//! The tests copy a fixture into a temp project, run the full
//! `manim_lint::application::check` pipeline, and assert the exact
//! diagnostic set (rule, line, column, severity, confidence) per file.

use std::path::{Path, PathBuf};

use manim_lint::application::check;
use manim_lint::cli::CheckArgs;
use manim_lint::diagnostic::{Confidence, Diagnostic, FixApplicability, Severity};
use manim_lint::reporting::OutputFormat;

/// One expected diagnostic, located by a source needle instead of a
/// hard-coded line/column so the fixtures stay editable.
struct Expected {
    rule: &'static str,
    /// Substring of the fixture whose `occurrence`-th match anchors the
    /// diagnostic; the span must start `offset` characters into it.
    needle: &'static str,
    occurrence: usize,
    offset: usize,
    severity: Severity,
    confidence: Confidence,
}

impl Expected {
    const fn new(
        rule: &'static str,
        needle: &'static str,
        occurrence: usize,
        offset: usize,
        severity: Severity,
        confidence: Confidence,
    ) -> Self {
        Self {
            rule,
            needle,
            occurrence,
            offset,
            severity,
            confidence,
        }
    }
}

fn error(rule: &'static str, needle: &'static str, occurrence: usize, offset: usize) -> Expected {
    Expected::new(
        rule,
        needle,
        occurrence,
        offset,
        Severity::Error,
        Confidence::High,
    )
}

fn warning(rule: &'static str, needle: &'static str, occurrence: usize, offset: usize) -> Expected {
    Expected::new(
        rule,
        needle,
        occurrence,
        offset,
        Severity::Warning,
        Confidence::High,
    )
}

fn info(rule: &'static str, needle: &'static str, occurrence: usize, offset: usize) -> Expected {
    Expected::new(
        rule,
        needle,
        occurrence,
        offset,
        Severity::Info,
        Confidence::High,
    )
}

fn fixture_root(rule: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/rules")
        .join(rule)
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Copies the fixture into a temp project, writes `pyproject.toml`, and
/// runs the whole check pipeline.
fn run_fixture(rule: &str, pyproject: &str) -> (tempfile::TempDir, Vec<Diagnostic>) {
    let project = tempfile::tempdir().unwrap();
    copy_dir(&fixture_root(rule), project.path());
    std::fs::write(project.path().join("pyproject.toml"), pyproject).unwrap();
    let args = CheckArgs {
        paths: vec![project.path().to_path_buf()],
        format: OutputFormat::Concise,
        ..CheckArgs::default()
    };
    let report = check(&args).expect("check pipeline must succeed");
    (project, report.diagnostics)
}

const DEFAULT_PYPROJECT: &str = "[tool.manim-lint]\n";

/// One-based (line, column) of the `occurrence`-th `needle` match plus
/// `offset` characters.
fn locate(text: &str, needle: &str, occurrence: usize, offset: usize) -> (usize, usize) {
    let mut from = 0;
    let mut seen = 0;
    while let Some(found) = text[from..].find(needle) {
        let at = from + found;
        seen += 1;
        if seen == occurrence {
            let target = at
                + needle
                    .char_indices()
                    .nth(offset)
                    .map_or(needle.len(), |(byte, _)| byte);
            let line = text[..target].matches('\n').count() + 1;
            let line_start = text[..target].rfind('\n').map_or(0, |index| index + 1);
            let column = text[line_start..target].chars().count() + 1;
            return (line, column);
        }
        from = at + needle.len();
    }
    panic!("needle {needle:?} occurrence {occurrence} not found");
}

/// Asserts that the diagnostics reported for `file` are exactly `expected`
/// (rule, line, column, severity, confidence), in stable order.
fn assert_file_diagnostics(
    project: &Path,
    diagnostics: &[Diagnostic],
    file: &str,
    expected: &[Expected],
) {
    let text = std::fs::read_to_string(project.join(file)).unwrap();
    let mut expected_tuples: Vec<(usize, usize, String, Severity, Confidence)> = expected
        .iter()
        .map(|expectation| {
            let (line, column) = locate(
                &text,
                expectation.needle,
                expectation.occurrence,
                expectation.offset,
            );
            (
                line,
                column,
                expectation.rule.to_owned(),
                expectation.severity,
                expectation.confidence,
            )
        })
        .collect();
    expected_tuples.sort_by(|a, b| (a.0, a.1, &a.2).cmp(&(b.0, b.1, &b.2)));
    let actual: Vec<(usize, usize, String, Severity, Confidence)> = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.path == file)
        .map(|diagnostic| {
            (
                diagnostic.primary_span.start.line,
                diagnostic.primary_span.start.column,
                diagnostic.rule_id.clone(),
                diagnostic.severity,
                diagnostic.confidence,
            )
        })
        .collect();
    assert_eq!(actual, expected_tuples, "diagnostics for {file}");
}

fn find<'a>(diagnostics: &'a [Diagnostic], file: &str, rule: &str, index: usize) -> &'a Diagnostic {
    diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.path == file && diagnostic.rule_id == rule)
        .nth(index)
        .expect("diagnostic present")
}

// ---------------------------------------------------------------------------
// MLR101
// ---------------------------------------------------------------------------

#[test]
fn mlr101_flags_confirmed_non_vmobject_targets_only() {
    let (project, diagnostics) = run_fixture("MLR101", DEFAULT_PYPROJECT);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "invalid.py",
        &[
            error("MLR101", "Create(img))", 1, 7),
            error("MLR101", "Write(5)", 1, 6),
            error("MLR101", "DrawBorderThenFill(Mobject())", 1, 19),
            error("MLR101", "Uncreate(Group(img))", 1, 9),
        ],
    );
    assert_file_diagnostics(project.path(), &diagnostics, "valid.py", &[]);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "alias.py",
        &[
            error("MLR101", "C(img)", 1, 2),
            error("MLR101", "mn.Write(5)", 1, 9),
        ],
    );
}

// ---------------------------------------------------------------------------
// MLR102
// ---------------------------------------------------------------------------

#[test]
fn mlr102_flags_played_bare_animate_with_unchanged_displayed_target() {
    let (project, diagnostics) = run_fixture("MLR102", DEFAULT_PYPROJECT);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "invalid.py",
        &[
            warning("MLR102", "square.animate)", 1, 0),
            warning("MLR102", "dot.animate", 1, 0),
        ],
    );
    // valid.py's stale-builder near miss is deliberately real MLC117
    // territory (see the fixture comment), verified against upstream
    // Manim: `_AnimationBuilder.__init__` runs `mobject.generate_target()`
    // the moment `.animate` is read, and `build()` produces a
    // `_MethodAnimation` (a `MoveToTarget`) toward that snapshot
    // (manim/mobject/mobject.py, manim/animation/transform.py). Mutating
    // the live mobject between builder creation and play therefore snaps
    // it back to the stale target — a true MLC117 finding, while MLR102
    // (a visual no-op) stays silent.
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "valid.py",
        &[warning("MLC117", "stale.animate", 1, 0)],
    );
    assert_file_diagnostics(project.path(), &diagnostics, "branch.py", &[]);
    assert_file_diagnostics(project.path(), &diagnostics, "suppressed.py", &[]);

    // The sole-argument play offers no deletion (an empty play() is a
    // runtime error); the two-argument play offers the UNSAFE deletion.
    let sole = find(&diagnostics, "invalid.py", "MLR102", 0);
    assert!(sole.fix.is_none());
    let paired = find(&diagnostics, "invalid.py", "MLR102", 1);
    let fix = paired.fix.as_ref().expect("deletion suggestion");
    assert_eq!(fix.applicability, FixApplicability::Unsafe);
    assert_eq!(fix.edits.len(), 1);
    assert_eq!(fix.edits[0].replacement, "");
}

// ---------------------------------------------------------------------------
// MLR103
// ---------------------------------------------------------------------------

#[test]
fn mlr103_flags_python_escape_tex_collisions_in_non_raw_literals() {
    let (project, diagnostics) = run_fixture("MLR103", DEFAULT_PYPROJECT);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "invalid.py",
        &[
            error("MLR103", "\"\\frac{a}{b}\"", 1, 0),
            error("MLR103", "\"x \\times y\"", 1, 0),
            error("MLR103", "\"\\alpha + \\tau\"", 1, 0),
        ],
    );
    assert_file_diagnostics(project.path(), &diagnostics, "valid.py", &[]);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "alias.py",
        &[
            error("MLR103", "\"\\frac{a}{b}\"", 1, 0),
            error("MLR103", "\"\\alpha\"", 1, 0),
        ],
    );

    // Import style must not change the outcome: the same literal fires
    // under star and aliased imports.
    let star = diagnostics
        .iter()
        .filter(|d| d.path == "invalid.py" && d.rule_id == "MLR103")
        .count();
    let aliased = diagnostics
        .iter()
        .filter(|d| d.path == "alias.py" && d.rule_id == "MLR103")
        .count();
    assert!(star >= 1 && aliased >= 1);

    // The raw-prefix suggestion is UNSAFE (it changes the runtime string).
    let first = find(&diagnostics, "invalid.py", "MLR103", 0);
    let fix = first.fix.as_ref().expect("MLR103 offers a suggestion");
    assert_eq!(fix.applicability, FixApplicability::Unsafe);
    assert_eq!(fix.edits.len(), 1);
    assert_eq!(fix.edits[0].replacement, "r");
    // Both escapes of the two-collision literal appear in the message.
    let both = find(&diagnostics, "invalid.py", "MLR103", 2);
    assert!(both.message.contains("alpha") && both.message.contains("tau"));
}

// ---------------------------------------------------------------------------
// MLR104
// ---------------------------------------------------------------------------

const MLR104_PYPROJECT: &str = "\
[tool.manim-lint]
default-profile = \"render\"

[[tool.manim-lint.profile]]
name = \"render\"
assets-dir = \"assets\"
";

#[test]
fn mlr104_resolves_assets_exactly_like_manim() {
    let (project, diagnostics) = run_fixture("MLR104", MLR104_PYPROJECT);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "invalid.py",
        &[
            error("MLR104", "\"missing.svg\"", 1, 0),
            error("MLR104", "\"absent\"", 1, 0),
            error("MLR104", "\"Logo.svg\"", 1, 0),
            error("MLR104", "\"Picture.png\"", 1, 0),
        ],
    );
    // MLR104 itself is silent on valid.py; the deliberately foreign
    // Windows path is unverifiable here (MLR104's near-miss) and instead
    // belongs to the platform-syntax rule MLD303 (Phase 4).
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "valid.py",
        &[Expected::new(
            "MLD303",
            "\"C:\\\\art\\\\logo.svg\"",
            1,
            0,
            Severity::Warning,
            Confidence::High,
        )],
    );
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "alias.py",
        &[
            error("MLR104", "\"missing.svg\"", 1, 0),
            error("MLR104", "\"also_missing.svg\"", 1, 0),
        ],
    );

    // The case-only mismatch carries a SAFE case correction and names the
    // profile it applies to.
    let case = diagnostics
        .iter()
        .find(|d| d.path == "invalid.py" && d.message.contains("Logo.svg"))
        .expect("case-only diagnostic");
    let fix = case.fix.as_ref().expect("safe case fix");
    assert_eq!(fix.applicability, FixApplicability::Safe);
    assert_eq!(fix.edits[0].replacement, "\"logo.svg\"");
    assert_eq!(case.applicable_profiles, vec!["render".to_owned()]);

    // The plain miss lists Manim's real search candidates as evidence.
    let missing = find(&diagnostics, "invalid.py", "MLR104", 0);
    let tried = missing.evidence.get("tried").expect("tried evidence");
    assert!(tried.to_string().contains("missing.svg"));
}

const MLR104_WINDOWS_PYPROJECT: &str = "\
[tool.manim-lint]
default-profile = \"win\"

[[tool.manim-lint.profile]]
name = \"win\"
platform = \"windows\"
assets-dir = \"assets\"
";

/// Case-only mismatches are silent when every declared target platform is
/// case-insensitive: the render's lookup on windows/macos finds the file
/// as written, so an error would claim a failure that happens on no
/// declared target (DESIGN §7.2 per-platform prose, AGENTS rule 4; DESIGN
/// §6 fixes one severity per rule ID, so no downgrade either). Genuinely
/// missing assets keep firing — their search fails on every platform.
#[test]
fn mlr104_case_only_mismatch_is_silent_on_case_insensitive_targets() {
    let (project, diagnostics) = run_fixture("MLR104", MLR104_WINDOWS_PYPROJECT);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "invalid.py",
        &[
            error("MLR104", "\"missing.svg\"", 1, 0),
            error("MLR104", "\"absent\"", 1, 0),
        ],
    );
}

/// An absolute path outside the project tree can only be checked against
/// the lint host's filesystem (DESIGN §7.2 prose decides validity by the
/// runtime's own search); the diagnostic declares that environment
/// dependence as evidence for downstream consumers.
#[test]
fn mlr104_absolute_out_of_project_path_declares_environment_dependence() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("scene.py"),
        "from manim import *\n\n\nclass Abs(Scene):\n    def construct(self):\n        \
         a = ImageMobject(\"/nonexistent-manim-lint-probe/pic.png\")\n",
    )
    .unwrap();
    std::fs::write(project.path().join("pyproject.toml"), MLR104_PYPROJECT).unwrap();
    let args = CheckArgs {
        paths: vec![project.path().to_path_buf()],
        format: OutputFormat::Concise,
        ..CheckArgs::default()
    };
    let report = check(&args).expect("check pipeline must succeed");
    let diagnostic = report
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.rule_id == "MLR104")
        .expect("unresolved absolute path fires MLR104");
    assert_eq!(
        diagnostic
            .evidence
            .get("environment_dependent")
            .map(ToString::to_string),
        Some("true".to_owned())
    );
}

// ---------------------------------------------------------------------------
// MLR105
// ---------------------------------------------------------------------------

#[test]
fn mlr105_flags_only_provable_pango_subset_errors() {
    let (project, diagnostics) = run_fixture("MLR105", DEFAULT_PYPROJECT);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "invalid.py",
        &[
            error("MLR105", "\"<b>bold</i>\"", 1, 0),
            error("MLR105", "\"<u>never closed\"", 1, 0),
            error("MLR105", "\"x &foo; y\"", 1, 0),
            error("MLR105", "\"stray </b> here\"", 1, 0),
        ],
    );
    assert_file_diagnostics(project.path(), &diagnostics, "valid.py", &[]);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "alias.py",
        &[
            error("MLR105", "\"<b>bold</i>\"", 1, 0),
            error("MLR105", "\"<u>never closed\"", 1, 0),
        ],
    );

    // The unclosed-tag case carries the UNSAFE closing-tag suggestion.
    let unclosed = diagnostics
        .iter()
        .find(|d| d.path == "invalid.py" && d.message.contains("never closed"))
        .expect("unclosed diagnostic");
    let fix = unclosed.fix.as_ref().expect("closing-tag suggestion");
    assert_eq!(fix.applicability, FixApplicability::Unsafe);
    assert_eq!(fix.edits[0].replacement, "</u>");
}

// ---------------------------------------------------------------------------
// MLR106
// ---------------------------------------------------------------------------

#[test]
fn mlr106_flags_confirmed_nan_inf_into_geometry() {
    let (project, diagnostics) = run_fixture("MLR106", DEFAULT_PYPROJECT);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "invalid.py",
        &[
            error("MLR106", "float(\"inf\")", 1, 0),
            error("MLR106", "float(\"nan\")", 1, 0),
            error("MLR106", "math.inf", 1, 0),
            error("MLR106", "move_to(inf)", 1, 8),
            error("MLR106", "float(\"-Inf\")", 1, 0),
            error("MLR106", "math.nan", 1, 0),
        ],
    );
    assert_file_diagnostics(project.path(), &diagnostics, "valid.py", &[]);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "alias.py",
        &[
            error("MLR106", "m.inf", 1, 0),
            error("MLR106", "shift(NAN)", 1, 6),
        ],
    );
}

// ---------------------------------------------------------------------------
// MLR113
// ---------------------------------------------------------------------------

#[test]
fn mlr113_flags_definitely_same_transform_source_and_target() {
    let (project, diagnostics) = run_fixture("MLR113", DEFAULT_PYPROJECT);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "invalid.py",
        &[
            info("MLR113", "Transform(square, square)", 1, 0),
            info("MLR113", "ReplacementTransform(square, square)", 1, 0),
        ],
    );
    assert_file_diagnostics(project.path(), &diagnostics, "valid.py", &[]);
    assert_file_diagnostics(project.path(), &diagnostics, "branch.py", &[]);
    assert_file_diagnostics(project.path(), &diagnostics, "suppressed.py", &[]);

    let first = find(&diagnostics, "invalid.py", "MLR113", 0);
    assert!(first.fix.is_none());
    assert!(first.message.contains("same object"));
}

// ---------------------------------------------------------------------------
// MLR114
// ---------------------------------------------------------------------------

#[test]
fn mlr114_flags_literal_points_rows_that_are_not_three_wide() {
    let (project, diagnostics) = run_fixture("MLR114", DEFAULT_PYPROJECT);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "invalid.py",
        &[
            error("MLR114", "[[0, 0], [1, 1], [2, 0]]", 1, 0),
            error(
                "MLR114",
                "[(0.0, 0.0, 0.0, 1.0), (1.0, 2.0, 3.0, 4.0)]",
                1,
                0,
            ),
        ],
    );
    assert_file_diagnostics(project.path(), &diagnostics, "valid.py", &[]);
    assert_file_diagnostics(project.path(), &diagnostics, "branch.py", &[]);
    assert_file_diagnostics(project.path(), &diagnostics, "suppressed.py", &[]);

    let first = find(&diagnostics, "invalid.py", "MLR114", 0);
    assert!(first.message.contains("(N, 3)"));
    assert_eq!(
        first.evidence.get("row_lengths").map(ToString::to_string),
        Some("[2,2,2]".to_owned())
    );
}

// ---------------------------------------------------------------------------
// MLR115
// ---------------------------------------------------------------------------

#[test]
fn mlr115_flags_literal_non_positive_font_size() {
    let certain = |needle, occurrence, offset| {
        Expected::new(
            "MLR115",
            needle,
            occurrence,
            offset,
            Severity::Error,
            Confidence::Certain,
        )
    };
    let (project, diagnostics) = run_fixture("MLR115", DEFAULT_PYPROJECT);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "invalid.py",
        &[
            certain("font_size=0)", 1, 10),
            certain("font_size=-12", 1, 10),
            certain("font_size=0.0", 1, 10),
            certain("font_size=-1.5", 1, 10),
        ],
    );
    assert_file_diagnostics(project.path(), &diagnostics, "valid.py", &[]);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "alias.py",
        &[
            certain("font_size=0)", 1, 10),
            certain("font_size=-3", 1, 10),
        ],
    );
}

// ---------------------------------------------------------------------------
// MLR117
// ---------------------------------------------------------------------------

#[test]
fn mlr117_flags_bare_register_font_statements_only() {
    let (project, diagnostics) = run_fixture("MLR117", DEFAULT_PYPROJECT);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "invalid.py",
        &[
            error("MLR117", "register_font(\"fonts/custom.ttf\")", 1, 0),
            error("MLR117", "register_font(\"module_level.ttf\")", 1, 0),
        ],
    );
    assert_file_diagnostics(project.path(), &diagnostics, "valid.py", &[]);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "alias.py",
        &[
            error("MLR117", "text_module.register_font(\"a.ttf\")", 1, 0),
            error("MLR117", "rf(\"b.ttf\")", 1, 0),
        ],
    );

    // The with-wrap suggestion is UNSAFE and produces parseable code.
    let bare = find(&diagnostics, "invalid.py", "MLR117", 0);
    let fix = bare.fix.as_ref().expect("with-wrap suggestion");
    assert_eq!(fix.applicability, FixApplicability::Unsafe);
    assert!(
        fix.edits[0]
            .replacement
            .starts_with("with register_font(\"fonts/custom.ttf\"):")
    );
    assert!(fix.edits[0].replacement.ends_with("pass"));
}

#[test]
fn mlr117_resolves_register_font_through_the_star_export() {
    // `register_font` is star-exported by Manim (`text_mobject.__all__`);
    // the knowledge profile export makes the plain `from manim import
    // register_font` spelling resolve (Wave 2 gap).
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("pyproject.toml"), DEFAULT_PYPROJECT).unwrap();
    std::fs::write(
        project.path().join("scene.py"),
        "from manim import register_font\n\nregister_font(\"custom.ttf\")\n",
    )
    .unwrap();
    let args = CheckArgs {
        paths: vec![project.path().to_path_buf()],
        format: OutputFormat::Concise,
        ..CheckArgs::default()
    };
    let report = check(&args).expect("check pipeline must succeed");
    let rules: Vec<&str> = report
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.rule_id.as_str())
        .collect();
    assert_eq!(rules, vec!["MLR117"]);
}

// ---------------------------------------------------------------------------
// MLR124
// ---------------------------------------------------------------------------

#[test]
fn mlr124_flags_matched_pango_pairs_in_plain_text() {
    let warning = |needle, occurrence, offset| {
        Expected::new(
            "MLR124",
            needle,
            occurrence,
            offset,
            Severity::Warning,
            Confidence::High,
        )
    };
    let (project, diagnostics) = run_fixture("MLR124", DEFAULT_PYPROJECT);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "invalid.py",
        &[
            warning("\"<b>bold</b> move\"", 1, 0),
            warning("\"mix <span foreground='red'>red</span> in\"", 1, 0),
        ],
    );
    assert_file_diagnostics(project.path(), &diagnostics, "valid.py", &[]);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "alias.py",
        &[
            warning("\"<b>bold</b>\"", 1, 0),
            warning("\"<u>underline</u>\"", 1, 0),
        ],
    );

    // The class switch is UNSAFE and rewrites only the callee.
    let first = find(&diagnostics, "invalid.py", "MLR124", 0);
    let fix = first.fix.as_ref().expect("MarkupText suggestion");
    assert_eq!(fix.applicability, FixApplicability::Unsafe);
    assert_eq!(fix.edits[0].replacement, "MarkupText");
    let module_alias = find(&diagnostics, "alias.py", "MLR124", 0);
    let fix = module_alias.fix.as_ref().expect("dotted callee suggestion");
    assert_eq!(fix.edits[0].replacement, "mn.MarkupText");
    // `Label(...)` cannot be rewritten to a resolvable name: no fix.
    let renamed = find(&diagnostics, "alias.py", "MLR124", 1);
    assert!(renamed.fix.is_none());
}

// ---------------------------------------------------------------------------
// MLR125
// ---------------------------------------------------------------------------

#[test]
fn mlr125_flags_bare_mobject_leaf_added_for_display() {
    let (project, diagnostics) = run_fixture("MLR125", DEFAULT_PYPROJECT);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "invalid.py",
        &[
            info("MLR125", "self.add(anchor)", 1, 0),
            info("MLR125", "self.add(marker)", 1, 0),
        ],
    );
    assert_file_diagnostics(project.path(), &diagnostics, "valid.py", &[]);
    assert_file_diagnostics(project.path(), &diagnostics, "branch.py", &[]);
    assert_file_diagnostics(project.path(), &diagnostics, "suppressed.py", &[]);

    // The message points at the legitimate container use and the
    // suppression escape hatch.
    let first = find(&diagnostics, "invalid.py", "MLR125", 0);
    assert!(first.message.contains("ignore[MLR125]"));
    assert!(first.fix.is_none());
}

// ---------------------------------------------------------------------------
// MLR126
// ---------------------------------------------------------------------------

#[test]
fn mlr126_flags_literal_opacity_and_stroke_width_ranges() {
    let (project, diagnostics) = run_fixture("MLR126", DEFAULT_PYPROJECT);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "invalid.py",
        &[
            error("MLR126", "fill_opacity=1.5", 1, 13),
            error("MLR126", "stroke_opacity=-0.1", 1, 15),
            error("MLR126", "stroke_width=-2", 1, 13),
            error("MLR126", "set_opacity(2.0)", 1, 12),
            error("MLR126", "set_fill(color, 1.2)", 1, 16),
            error("MLR126", "width=-3", 1, 6),
            error("MLR126", "opacity=1.01", 1, 8),
        ],
    );
    assert_file_diagnostics(project.path(), &diagnostics, "valid.py", &[]);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "alias.py",
        &[
            error("MLR126", "fill_opacity=2", 1, 13),
            error("MLR126", "stroke_width=-1", 1, 13),
        ],
    );
}

// ---------------------------------------------------------------------------
// MLR127
// ---------------------------------------------------------------------------

#[test]
fn mlr127_flags_by_tex_keys_absent_from_the_literal() {
    let (project, diagnostics) = run_fixture("MLR127", DEFAULT_PYPROJECT);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "invalid.py",
        &[
            warning("MLR127", "\"c^2\"", 1, 0),
            warning("MLR127", "\"x\"", 1, 0),
            warning("MLR127", "\"goodbye\"", 1, 0),
        ],
    );
    assert_file_diagnostics(project.path(), &diagnostics, "valid.py", &[]);
    assert_file_diagnostics(project.path(), &diagnostics, "branch.py", &[]);
    assert_file_diagnostics(project.path(), &diagnostics, "suppressed.py", &[]);

    // The message suggests isolating a real substring.
    let first = find(&diagnostics, "invalid.py", "MLR127", 0);
    assert!(
        first
            .explanation
            .as_deref()
            .unwrap_or("")
            .contains("substrings_to_isolate")
    );
    let parts = first.evidence.get("tex_arguments").expect("tex parts");
    assert!(parts.to_string().contains("a^2"));
}
