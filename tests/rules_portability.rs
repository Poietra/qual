//! Golden tests for the Phase 4 determinism / portability rules
//! (`MLD301`-`MLD307`, DESIGN §7.4).
//!
//! Each rule has a fixture directory under `tests/fixtures/rules/<ID>/`
//! (DESIGN §11.1): `invalid.py` (true positives), `valid.py` (near-misses
//! that must stay silent), `branch.py` (an Unknown / Maybe case that must
//! silence the rule), and `suppressed.py` (inline suppression). Fixture
//! pyprojects select only the rule under test so the assertions stay
//! independent of the other rule groups, and set `min-confidence = "low"`
//! where the rule's minimum confidence is `medium` (`MLD302` / `MLD304` /
//! `MLD307`).
//!
//! The tests copy a fixture into a temp project, run the full
//! `manim_lint::application::check` pipeline, and assert the exact
//! diagnostic set (rule, line, column, severity, confidence) per file.

use std::path::{Path, PathBuf};

use manim_lint::application::check;
use manim_lint::cli::CheckArgs;
use manim_lint::diagnostic::{Confidence, Diagnostic, Severity};
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

fn warning_high(rule: &'static str, needle: &'static str) -> Expected {
    Expected::new(rule, needle, 1, 0, Severity::Warning, Confidence::High)
}

fn warning_medium(rule: &'static str, needle: &'static str) -> Expected {
    Expected::new(rule, needle, 1, 0, Severity::Warning, Confidence::Medium)
}

fn info_high(rule: &'static str, needle: &'static str) -> Expected {
    Expected::new(rule, needle, 1, 0, Severity::Info, Confidence::High)
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
/// runs the whole check pipeline (optionally with `--profile`).
fn run_fixture_with_profile(
    rule: &str,
    pyproject: &str,
    profile: Option<&str>,
) -> (tempfile::TempDir, Vec<Diagnostic>) {
    let project = tempfile::tempdir().unwrap();
    copy_dir(&fixture_root(rule), project.path());
    std::fs::write(project.path().join("pyproject.toml"), pyproject).unwrap();
    let args = CheckArgs {
        paths: vec![project.path().to_path_buf()],
        format: OutputFormat::Concise,
        profile: profile.map(str::to_owned),
        ..CheckArgs::default()
    };
    let report = check(&args).expect("check pipeline must succeed");
    (project, report.diagnostics)
}

fn run_fixture(rule: &str, pyproject: &str) -> (tempfile::TempDir, Vec<Diagnostic>) {
    run_fixture_with_profile(rule, pyproject, None)
}

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
// MLD301
// ---------------------------------------------------------------------------

const MLD301_PYPROJECT: &str = "\
[tool.manim-lint]
select = [\"MLD301\"]
";

#[test]
fn mld301_flags_fixed_step_updater_mutations_without_dt() {
    let (project, diagnostics) = run_fixture("MLD301", MLD301_PYPROJECT);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "invalid.py",
        &[
            warning_high("MLD301", "m.shift(0.1 * RIGHT)"),
            warning_high("MLD301", "m.rotate(0.05)"),
            warning_high("MLD301", "m.increment_value(0.1)"),
            warning_high("MLD301", "mob.scale(1.01)"),
        ],
    );
    assert_file_diagnostics(project.path(), &diagnostics, "valid.py", &[]);
    assert_file_diagnostics(project.path(), &diagnostics, "branch.py", &[]);
    assert_file_diagnostics(project.path(), &diagnostics, "suppressed.py", &[]);

    // The registration site is attached as a related location.
    let first = find(&diagnostics, "invalid.py", "MLD301", 0);
    assert_eq!(first.related_locations.len(), 1);
    assert!(first.related_locations[0].message.contains("registered"));
    assert_eq!(
        first.evidence.get("declares_dt").map(ToString::to_string),
        Some("false".to_owned())
    );
}

// ---------------------------------------------------------------------------
// MLD302
// ---------------------------------------------------------------------------

const MLD302_PYPROJECT: &str = "\
[tool.manim-lint]
select = [\"MLD302\"]
min-confidence = \"low\"
";

#[test]
fn mld302_flags_unseeded_global_random_in_hot_contexts_only() {
    let (project, diagnostics) = run_fixture("MLD302", MLD302_PYPROJECT);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "invalid.py",
        &[
            warning_medium("MLD302", "random.uniform(-1.0, 1.0)"),
            warning_medium("MLD302", "np.random.uniform(-1.0, 1.0)"),
            warning_medium("MLD302", "choice(colors)"),
        ],
    );
    // Seeded local generators and cold-context reads stay silent.
    assert_file_diagnostics(project.path(), &diagnostics, "valid.py", &[]);
    // A module-level seed downgrades the whole file to silence.
    assert_file_diagnostics(project.path(), &diagnostics, "branch.py", &[]);
    assert_file_diagnostics(project.path(), &diagnostics, "suppressed.py", &[]);

    let first = find(&diagnostics, "invalid.py", "MLD302", 0);
    assert_eq!(
        first.evidence.get("function").map(ToString::to_string),
        Some("\"random.uniform\"".to_owned())
    );
    assert_eq!(
        first.evidence.get("entry").map(ToString::to_string),
        Some("\"updater\"".to_owned())
    );
}

// ---------------------------------------------------------------------------
// MLD303
// ---------------------------------------------------------------------------

const MLD303_PYPROJECT: &str = "\
[tool.manim-lint]
select = [\"MLD303\"]
";

const MLD303_WINDOWS_PYPROJECT: &str = "\
[tool.manim-lint]
select = [\"MLD303\"]
default-profile = \"win\"

[[tool.manim-lint.profile]]
name = \"win\"
platform = \"windows\"
";

#[test]
fn mld303_flags_windows_paths_under_the_default_linux_platform() {
    let (project, diagnostics) = run_fixture("MLD303", MLD303_PYPROJECT);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "invalid.py",
        &[
            warning_high("MLD303", "\"C:\\\\assets\\\\logo.svg\""),
            warning_high("MLD303", "r\"C:\\icons\\icon.svg\""),
            warning_high("MLD303", "\"D:/pictures/photo.png\""),
            warning_high("MLD303", "\"art\\\\shape.svg\""),
        ],
    );
    assert_file_diagnostics(project.path(), &diagnostics, "valid.py", &[]);
    assert_file_diagnostics(project.path(), &diagnostics, "branch.py", &[]);
    assert_file_diagnostics(project.path(), &diagnostics, "suppressed.py", &[]);

    let first = find(&diagnostics, "invalid.py", "MLD303", 0);
    assert_eq!(first.applicable_profiles, vec!["default".to_owned()]);
    assert_eq!(
        first.evidence.get("path_syntax").map(ToString::to_string),
        Some("\"windows\"".to_owned())
    );
}

#[test]
fn mld303_flags_posix_absolute_paths_under_a_windows_platform() {
    let (project, diagnostics) = run_fixture("MLD303", MLD303_WINDOWS_PYPROJECT);
    // Windows-style paths are fine on the windows platform.
    assert_file_diagnostics(project.path(), &diagnostics, "invalid.py", &[]);
    // ... and the POSIX absolute path now mismatches ("vice versa").
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "valid.py",
        &[warning_high("MLD303", "\"/usr/share/icons/logo.svg\"")],
    );
    let posix = find(&diagnostics, "valid.py", "MLD303", 0);
    assert_eq!(posix.applicable_profiles, vec!["win".to_owned()]);
}

// ---------------------------------------------------------------------------
// MLD304
// ---------------------------------------------------------------------------

const MLD304_PYPROJECT: &str = "\
[tool.manim-lint]
select = [\"MLD304\"]
min-confidence = \"low\"
default-profile = \"cairo\"

[[tool.manim-lint.profile]]
name = \"cairo\"
renderer = \"cairo\"

[[tool.manim-lint.profile]]
name = \"opengl\"
renderer = \"opengl\"
";

#[test]
fn mld304_fires_only_when_the_run_targets_both_renderers() {
    // `--profile all`: the run targets cairo and opengl.
    let (project, diagnostics) = run_fixture_with_profile("MLD304", MLD304_PYPROJECT, Some("all"));
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "invalid.py",
        &[warning_medium(
            "MLD304",
            "self.remove_fixed_in_frame_mobjects(label)",
        )],
    );
    assert_file_diagnostics(project.path(), &diagnostics, "valid.py", &[]);
    // A branch around the call (a renderer guard included) means Maybe:
    // silence.
    assert_file_diagnostics(project.path(), &diagnostics, "branch.py", &[]);
    assert_file_diagnostics(project.path(), &diagnostics, "suppressed.py", &[]);

    let divergent = find(&diagnostics, "invalid.py", "MLD304", 0);
    assert!(divergent.message.contains("cairo"));
    assert!(divergent.message.contains("opengl"));

    // A single-renderer run never asks for renderer guards.
    let (_project, single) = run_fixture("MLD304", MLD304_PYPROJECT);
    assert!(
        single.is_empty(),
        "single-renderer run must be silent: {single:?}"
    );
}

// ---------------------------------------------------------------------------
// MLD305
// ---------------------------------------------------------------------------

const MLD305_PYPROJECT: &str = "\
[tool.manim-lint]
select = [\"MLD305\"]
default-profile = \"render\"

[[tool.manim-lint.profile]]
name = \"render\"
assets-dir = \"assets\"
";

const MLD305_WINDOWS_PYPROJECT: &str = "\
[tool.manim-lint]
select = [\"MLD305\"]
default-profile = \"win\"

[[tool.manim-lint.profile]]
name = \"win\"
platform = \"windows\"
assets-dir = \"assets\"
";

#[test]
fn mld305_flags_case_only_mismatches_for_case_sensitive_targets() {
    let (project, diagnostics) = run_fixture("MLD305", MLD305_PYPROJECT);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "invalid.py",
        &[
            warning_high("MLD305", "\"ICON\""),
            warning_high("MLD305", "\"Picture\""),
        ],
    );
    // Near misses stay silent, including the literal-level case mismatch
    // that MLR104 owns (with its SAFE fix): one span, one finding.
    assert_file_diagnostics(project.path(), &diagnostics, "valid.py", &[]);
    assert_file_diagnostics(project.path(), &diagnostics, "branch.py", &[]);
    assert_file_diagnostics(project.path(), &diagnostics, "suppressed.py", &[]);

    // The evidence carries the on-disk spelling of the extension-augmented
    // candidate.
    let svg = find(&diagnostics, "invalid.py", "MLD305", 0);
    assert_eq!(
        svg.evidence.get("on_disk").map(ToString::to_string),
        Some("\"icon.svg\"".to_owned())
    );
    let raster = find(&diagnostics, "invalid.py", "MLD305", 1);
    assert_eq!(
        raster.evidence.get("on_disk").map(ToString::to_string),
        Some("\"picture.png\"".to_owned())
    );
    assert_eq!(svg.applicable_profiles, vec!["render".to_owned()]);
}

#[test]
fn mld305_stays_silent_for_case_insensitive_target_platforms() {
    let (_project, diagnostics) = run_fixture("MLD305", MLD305_WINDOWS_PYPROJECT);
    assert!(
        diagnostics.is_empty(),
        "windows targets mask case-only mismatches: {diagnostics:?}"
    );
}

const MLD305_WITH_MLR104_PYPROJECT: &str = "\
[tool.manim-lint]
select = [\"MLD305\", \"MLR104\"]
default-profile = \"render\"

[[tool.manim-lint.profile]]
name = \"render\"
assets-dir = \"assets\"
";

/// The extension-augmented case-only mismatch scenario produces BOTH rules
/// on the same literal span before dedup (`MLR104` sees an unresolved
/// path, `MLD305` identifies its case-only cause); the declared
/// `MLD305 supersedes MLR104` edge keeps only the specific finding, while
/// `MLR104` keeps its own territory (a literal-level case rewrite and a
/// genuinely missing file) untouched.
#[test]
fn mld305_supersedes_mlr104_on_the_shared_span() {
    let (project, diagnostics) = run_fixture("MLD305", MLD305_WITH_MLR104_PYPROJECT);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "invalid.py",
        &[
            warning_high("MLD305", "\"ICON\""),
            warning_high("MLD305", "\"Picture\""),
        ],
    );
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "valid.py",
        &[
            Expected::new(
                "MLR104",
                "\"Logo.svg\"",
                1,
                0,
                Severity::Error,
                Confidence::High,
            ),
            Expected::new(
                "MLR104",
                "\"absent.svg\"",
                1,
                0,
                Severity::Error,
                Confidence::High,
            ),
        ],
    );
}

/// Supersession is part of diagnostic production and runs before inline
/// suppression (see `application::check`): suppressing the specific
/// `MLD305` silences the finding entirely — the superseded generic
/// `MLR104` does NOT resurface at that span.
#[test]
fn suppressing_the_specific_rule_does_not_resurrect_the_superseded_generic() {
    let (_project, diagnostics) = run_fixture("MLD305", MLD305_WITH_MLR104_PYPROJECT);
    let suppressed: Vec<&Diagnostic> = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.path == "suppressed.py")
        .collect();
    assert!(
        suppressed.is_empty(),
        "ignore[MLD305] must silence the whole finding: {suppressed:?}"
    );
}

// ---------------------------------------------------------------------------
// MLD306
// ---------------------------------------------------------------------------

const MLD306_PYPROJECT: &str = "\
[tool.manim-lint]
select = [\"MLD306\"]
default-profile = \"prod\"

[[tool.manim-lint.profile]]
name = \"prod\"
allowed-fonts = [\"Noto Sans\", \"Noto Sans CJK JP\"]
";

const MLD306_NO_ALLOWLIST_PYPROJECT: &str = "\
[tool.manim-lint]
select = [\"MLD306\"]
";

#[test]
fn mld306_flags_fonts_outside_a_configured_allowlist() {
    let (project, diagnostics) = run_fixture("MLD306", MLD306_PYPROJECT);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "invalid.py",
        &[
            info_high("MLD306", "\"Comic Sans MS\""),
            info_high("MLD306", "\"Papyrus\""),
        ],
    );
    assert_file_diagnostics(project.path(), &diagnostics, "valid.py", &[]);
    assert_file_diagnostics(project.path(), &diagnostics, "branch.py", &[]);
    assert_file_diagnostics(project.path(), &diagnostics, "suppressed.py", &[]);

    let first = find(&diagnostics, "invalid.py", "MLD306", 0);
    assert_eq!(first.applicable_profiles, vec!["prod".to_owned()]);
}

#[test]
fn mld306_stays_silent_without_an_allowlist() {
    // An empty allowed-fonts list means "no allowlist configured".
    let (_project, diagnostics) = run_fixture("MLD306", MLD306_NO_ALLOWLIST_PYPROJECT);
    assert!(
        diagnostics.is_empty(),
        "no allowlist means silence: {diagnostics:?}"
    );
}

// ---------------------------------------------------------------------------
// MLD307
// ---------------------------------------------------------------------------

const MLD307_PYPROJECT: &str = "\
[tool.manim-lint]
select = [\"MLD307\"]
min-confidence = \"low\"
";

#[test]
fn mld307_flags_wall_clock_filesystem_and_network_io_in_frame_callbacks() {
    let (project, diagnostics) = run_fixture("MLD307", MLD307_PYPROJECT);
    assert_file_diagnostics(
        project.path(),
        &diagnostics,
        "invalid.py",
        &[
            warning_medium("MLD307", "time.time()"),
            warning_medium("MLD307", "datetime.now()"),
            warning_medium("MLD307", "Path(\"data.txt\").read_text()"),
            warning_medium("MLD307", "open(\"data.txt\")"),
            warning_medium("MLD307", "requests.get(\"http://example.com\")"),
            warning_medium("MLD307", "socket.gethostbyname(\"example.com\")"),
        ],
    );
    assert_file_diagnostics(project.path(), &diagnostics, "valid.py", &[]);
    // Rebinding the module name anywhere in the file means silence.
    assert_file_diagnostics(project.path(), &diagnostics, "branch.py", &[]);
    assert_file_diagnostics(project.path(), &diagnostics, "suppressed.py", &[]);

    let clock = find(&diagnostics, "invalid.py", "MLD307", 0);
    assert_eq!(
        clock.evidence.get("category").map(ToString::to_string),
        Some("\"wall-clock\"".to_owned())
    );
    let network = find(&diagnostics, "invalid.py", "MLD307", 4);
    assert_eq!(
        network.evidence.get("category").map(ToString::to_string),
        Some("\"network\"".to_owned())
    );
}
