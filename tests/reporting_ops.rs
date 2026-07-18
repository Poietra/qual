//! Operational reporting tests (DESIGN §6.3, §8.1, §8.3 and Phase 5 items
//! pulled forward): SARIF structure and determinism, baseline write /
//! filter / robustness, and fix application (overlap rejection, rollback,
//! idempotence, Unicode spans).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use manim_lint::application::check;
use manim_lint::cli::{CheckArgs, ExitStatus};
use manim_lint::diagnostic::{
    Confidence, Diagnostic, Fix, FixApplicability, Severity, SourcePosition, SourceSpan, TextEdit,
};
use manim_lint::reporting::{OutputFormat, fixes};
use manim_lint::source::SourceManager;
use serde_json::Value;

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

/// Valid scene whose unknown suppression ID yields an `MLC001` warning on
/// line 6, with non-blank neighbor statements two lines away on each side.
const GOOD_SCENE: &str = "\
\"\"\"Valid scene.\"\"\"

a = 1
b = 2

value = 3  # manim-lint: ignore[MLC999]

c = 4
d = 5
";

/// Japanese text before the syntax error exercises character columns.
const BAD_SCENE: &str = "x = 1\ny = \"こんにちは\"; def = 1\n";

fn write_project(root: &Path) {
    std::fs::write(root.join("pyproject.toml"), PYPROJECT).unwrap();
    std::fs::create_dir_all(root.join("scenes")).unwrap();
    std::fs::write(root.join("scenes/good.py"), GOOD_SCENE).unwrap();
    std::fs::write(root.join("scenes/bad.py"), BAD_SCENE).unwrap();
}

fn args_for(root: &Path, format: OutputFormat) -> CheckArgs {
    CheckArgs {
        paths: vec![root.to_path_buf()],
        format,
        ..CheckArgs::default()
    }
}

// --------------------------------------------------------------------------
// SARIF
// --------------------------------------------------------------------------

#[test]
fn sarif_output_has_required_structure() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path());
    let report = check(&args_for(project.path(), OutputFormat::Sarif)).unwrap();

    let value: Value = serde_json::from_str(&report.output).expect("valid SARIF JSON");
    assert_eq!(value["version"], "2.1.0");
    let run = &value["runs"][0];
    assert_eq!(run["columnKind"], "unicodeCodePoints");

    let driver = &run["tool"]["driver"];
    assert_eq!(driver["name"], "manim-lint");
    assert!(driver["version"].is_string());
    assert!(driver["informationUri"].is_string());

    let rules = driver["rules"].as_array().expect("rules array");
    let results = run["results"].as_array().expect("results array");
    assert!(!results.is_empty());
    for result in results {
        let rule_id = result["ruleId"].as_str().expect("ruleId");
        assert!(
            rules
                .iter()
                .any(|rule| rule["id"] == rule_id && rule["shortDescription"]["text"].is_string()),
            "every reported rule needs a descriptor with shortDescription"
        );
        assert!(
            ["error", "warning", "note"].contains(&result["level"].as_str().expect("level")),
            "invalid SARIF level"
        );
        assert!(result["message"]["text"].is_string());

        let physical = &result["locations"][0]["physicalLocation"];
        let uri = physical["artifactLocation"]["uri"].as_str().expect("uri");
        assert!(!uri.contains('\\'), "URIs must be POSIX");
        assert!(!uri.starts_with('/'), "URIs must be project-relative");
        let region = &physical["region"];
        for key in ["startLine", "startColumn", "endLine", "endColumn"] {
            assert!(region[key].as_u64().expect(key) >= 1, "1-based {key}");
        }
    }

    // Severity mapping: error -> error, warning -> warning.
    assert!(
        results
            .iter()
            .any(|result| result["ruleId"] == "MLC000" && result["level"] == "error")
    );
    assert!(
        results
            .iter()
            .any(|result| result["ruleId"] == "MLC001" && result["level"] == "warning")
    );
    // The MLC000 result points at the character (not byte) column after the
    // Japanese text in bad.py.
    let broken = results
        .iter()
        .find(|result| result["ruleId"] == "MLC000")
        .expect("MLC000 result");
    let region = &broken["locations"][0]["physicalLocation"]["region"];
    assert_eq!(region["startLine"], 2);
    assert_eq!(region["startColumn"], 14);
}

#[test]
fn sarif_output_is_byte_deterministic() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path());
    let first = check(&args_for(project.path(), OutputFormat::Sarif)).unwrap();
    let second = check(&args_for(project.path(), OutputFormat::Sarif)).unwrap();
    assert_eq!(first.output.as_bytes(), second.output.as_bytes());
}

// --------------------------------------------------------------------------
// Baseline
// --------------------------------------------------------------------------

#[test]
fn baseline_write_filter_and_line_insertion_survival() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path());
    let baseline_path = project.path().join("baseline.json");

    // Write the baseline; exit follows normal semantics.
    let mut args = args_for(project.path(), OutputFormat::Concise);
    args.write_baseline = Some(baseline_path.clone());
    let report = check(&args).unwrap();
    assert!(!report.diagnostics.is_empty());
    assert_eq!(report.exit, ExitStatus::Failure, "MLC000 still fails");

    // The written file matches the v1 shape: schema_version 1, sorted
    // entries with the four fingerprint fields.
    let text = std::fs::read_to_string(&baseline_path).unwrap();
    let value: Value = serde_json::from_str(&text).expect("valid baseline JSON");
    assert_eq!(value["schema_version"], 1);
    let entries = value["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), report.diagnostics.len());
    for entry in entries {
        for key in ["rule_id", "path", "scene", "token_hash"] {
            assert!(entry.get(key).is_some(), "missing entry key {key}");
        }
        // This fixture has no Scene classes, so every diagnostic is
        // outside any scene.
        assert_eq!(entry["scene"], "", "no enclosing Scene class");
        assert!(
            entry["token_hash"]
                .as_str()
                .expect("token_hash")
                .starts_with("fnv1a64:")
        );
        assert!(entry.get("line").is_none(), "fingerprints hold no lines");
    }

    // Writing again is byte-identical.
    let again = project.path().join("baseline2.json");
    let mut args = args_for(project.path(), OutputFormat::Concise);
    args.write_baseline = Some(again.clone());
    check(&args).unwrap();
    assert_eq!(
        std::fs::read(&baseline_path).unwrap(),
        std::fs::read(&again).unwrap(),
        "baseline output must be byte-stable"
    );

    // Re-check against the baseline: everything known is filtered out
    // before rendering and exit-code computation.
    let mut args = args_for(project.path(), OutputFormat::Concise);
    args.baseline = Some(baseline_path.clone());
    let report = check(&args).unwrap();
    assert!(report.diagnostics.is_empty(), "all diagnostics are known");
    assert_eq!(report.exit, ExitStatus::Success);
    assert_eq!(report.output, "", "nothing rendered");

    // Insert an unrelated line above the diagnostic statement (its own
    // neighbor lines are untouched): the fingerprints hold no line numbers
    // so every entry still matches.
    let good = project.path().join("scenes/good.py");
    let text = std::fs::read_to_string(&good).unwrap();
    std::fs::write(&good, text.replace("a = 1", "unrelated = 0\na = 1")).unwrap();
    let report = check(&args).unwrap();
    assert!(
        report.diagnostics.is_empty(),
        "inserting an unrelated line must not invalidate the baseline"
    );
    assert_eq!(report.exit, ExitStatus::Success);
}

/// Two Scene classes with token-identical `construct` bodies: without
/// scene attribution their fingerprints would collide.
const TWIN_SCENES: &str = "\
from manim import *


class SceneA(Scene):
    def construct(self):
        self.play()
        self.wait(1)


class SceneB(Scene):
    def construct(self):
        self.play()
        self.wait(1)
";

#[test]
fn identical_findings_in_two_scenes_get_distinct_fingerprints() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("demo.py"), TWIN_SCENES).unwrap();
    let baseline_path = project.path().join("baseline.json");

    let mut args = args_for(project.path(), OutputFormat::Concise);
    args.write_baseline = Some(baseline_path.clone());
    let report = check(&args).unwrap();
    let empty_plays = report
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.rule_id == "MLC101")
        .count();
    assert_eq!(empty_plays, 2, "one empty play per scene");

    let text = std::fs::read_to_string(&baseline_path).unwrap();
    let value: Value = serde_json::from_str(&text).expect("valid baseline JSON");
    let entries: Vec<&Value> = value["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .filter(|entry| entry["rule_id"] == "MLC101")
        .collect();
    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries[0]["token_hash"], entries[1]["token_hash"],
        "the surrounding tokens are identical"
    );
    assert_eq!(entries[0]["scene"], "demo.SceneA");
    assert_eq!(entries[1]["scene"], "demo.SceneB");

    // Round trip: the scene-aware baseline suppresses everything, and
    // re-writing it is byte-identical.
    let again = project.path().join("baseline2.json");
    let mut args = args_for(project.path(), OutputFormat::Concise);
    args.baseline = Some(baseline_path.clone());
    args.write_baseline = Some(again.clone());
    let report = check(&args).unwrap();
    assert!(report.diagnostics.is_empty(), "all diagnostics are known");
    assert_eq!(
        std::fs::read(&baseline_path).unwrap(),
        std::fs::read(&again).unwrap(),
        "scene-aware baseline output must be byte-stable"
    );
}

#[test]
fn pre_attribution_baseline_with_empty_scenes_still_suppresses() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("demo.py"), TWIN_SCENES).unwrap();
    let baseline_path = project.path().join("baseline.json");

    let mut args = args_for(project.path(), OutputFormat::Concise);
    args.write_baseline = Some(baseline_path.clone());
    check(&args).unwrap();

    // Rewrite the baseline the way a pre-attribution build would have
    // written it: every scene field blanked.
    let text = std::fs::read_to_string(&baseline_path).unwrap();
    let mut value: Value = serde_json::from_str(&text).expect("valid baseline JSON");
    for entry in value["entries"].as_array_mut().expect("entries") {
        entry["scene"] = Value::String(String::new());
    }
    std::fs::write(&baseline_path, serde_json::to_string(&value).unwrap()).unwrap();

    let mut args = args_for(project.path(), OutputFormat::Concise);
    args.baseline = Some(baseline_path);
    let report = check(&args).unwrap();
    assert!(
        report.diagnostics.is_empty(),
        "stored empty scenes act as wildcards: {:?}",
        report.diagnostics
    );
    assert_eq!(report.exit, ExitStatus::Success);
}

#[test]
fn corrupt_or_wrong_schema_baseline_is_an_error() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path());
    let baseline_path = project.path().join("baseline.json");

    std::fs::write(&baseline_path, "this is not json {{").unwrap();
    let mut args = args_for(project.path(), OutputFormat::Concise);
    args.baseline = Some(baseline_path.clone());
    let error = check(&args).expect_err("corrupt baseline must be exit 2");
    assert!(error.to_string().contains("baseline"));

    std::fs::write(&baseline_path, r#"{"schema_version": 2, "entries": []}"#).unwrap();
    let error = check(&args).expect_err("wrong schema_version must be exit 2");
    assert!(error.to_string().contains("schema_version"));
}

// --------------------------------------------------------------------------
// Fix application
// --------------------------------------------------------------------------

fn load_sources(root: &Path, files: &[(&str, &str)]) -> SourceManager {
    let mut sources = SourceManager::new(root);
    for (name, text) in files {
        let path = root.join(name);
        std::fs::write(&path, text).unwrap();
        sources.load_file(&path);
    }
    sources
}

fn span(line: usize, start_column: usize, end_column: usize) -> SourceSpan {
    SourceSpan {
        start: SourcePosition {
            line,
            column: start_column,
        },
        end: SourcePosition {
            line,
            column: end_column,
        },
    }
}

fn diagnostic_with_fix(
    path: &str,
    edits: Vec<TextEdit>,
    applicability: FixApplicability,
) -> Diagnostic {
    Diagnostic {
        rule_id: "MLC101".to_owned(),
        severity: Severity::Error,
        confidence: Confidence::Certain,
        path: path.to_owned(),
        primary_span: edits
            .first()
            .map_or_else(|| span(1, 1, 2), |edit| edit.span),
        message: "test fix".to_owned(),
        explanation: None,
        related_locations: Vec::new(),
        evidence: BTreeMap::new(),
        estimated_cost: None,
        applicable_profiles: Vec::new(),
        fix: Some(Fix {
            applicability,
            message: "test fix".to_owned(),
            edits,
        }),
    }
}

fn edit(path: &str, edit_span: SourceSpan, replacement: &str) -> TextEdit {
    TextEdit {
        path: path.to_owned(),
        span: edit_span,
        replacement: replacement.to_owned(),
    }
}

#[test]
fn safe_fix_is_applied_and_file_reparses() {
    let project = tempfile::tempdir().unwrap();
    let sources = load_sources(project.path(), &[("scene.py", "flag = False\n")]);
    let diagnostic = diagnostic_with_fix(
        "scene.py",
        vec![edit("scene.py", span(1, 8, 13), "True")],
        FixApplicability::Safe,
    );

    let report = fixes::apply(&sources, &[diagnostic], false).unwrap();
    assert_eq!(report.applied, 1);
    assert_eq!(report.files_changed, vec!["scene.py".to_owned()]);
    assert!(report.rolled_back.is_empty());
    assert_eq!(
        std::fs::read_to_string(project.path().join("scene.py")).unwrap(),
        "flag = True\n"
    );
}

#[test]
fn unsafe_fix_needs_the_unsafe_fixes_flag() {
    let project = tempfile::tempdir().unwrap();
    let sources = load_sources(project.path(), &[("scene.py", "flag = False\n")]);
    let diagnostic = diagnostic_with_fix(
        "scene.py",
        vec![edit("scene.py", span(1, 8, 13), "True")],
        FixApplicability::Unsafe,
    );

    let report = fixes::apply(&sources, std::slice::from_ref(&diagnostic), false).unwrap();
    assert_eq!(report.applied, 0);
    assert_eq!(report.skipped_unsafe, 1);
    assert_eq!(
        std::fs::read_to_string(project.path().join("scene.py")).unwrap(),
        "flag = False\n",
        "unsafe fixes never apply without --unsafe-fixes"
    );

    let report = fixes::apply(&sources, &[diagnostic], true).unwrap();
    assert_eq!(report.applied, 1);
    assert_eq!(
        std::fs::read_to_string(project.path().join("scene.py")).unwrap(),
        "flag = True\n"
    );
}

#[test]
fn overlapping_fix_is_skipped_whole() {
    let project = tempfile::tempdir().unwrap();
    let sources = load_sources(project.path(), &[("scene.py", "value = 1234567890\n")]);
    // First fix replaces columns 9..14, second overlaps it at 12..17.
    let first = diagnostic_with_fix(
        "scene.py",
        vec![edit("scene.py", span(1, 9, 14), "99999")],
        FixApplicability::Safe,
    );
    let second = diagnostic_with_fix(
        "scene.py",
        vec![edit("scene.py", span(1, 12, 17), "88888")],
        FixApplicability::Safe,
    );

    let report = fixes::apply(&sources, &[first, second], false).unwrap();
    assert_eq!(report.applied, 1);
    assert_eq!(report.skipped_overlapping, 1);
    assert_eq!(
        std::fs::read_to_string(project.path().join("scene.py")).unwrap(),
        "value = 9999967890\n",
        "only the first fix applies"
    );
}

#[test]
fn internally_overlapping_edits_skip_the_fix() {
    let project = tempfile::tempdir().unwrap();
    let sources = load_sources(project.path(), &[("scene.py", "value = 1234567890\n")]);
    let diagnostic = diagnostic_with_fix(
        "scene.py",
        vec![
            edit("scene.py", span(1, 9, 14), "99999"),
            edit("scene.py", span(1, 12, 17), "88888"),
        ],
        FixApplicability::Safe,
    );

    let report = fixes::apply(&sources, &[diagnostic], false).unwrap();
    assert_eq!(report.applied, 0);
    assert_eq!(report.skipped_overlapping, 1);
    assert_eq!(
        std::fs::read_to_string(project.path().join("scene.py")).unwrap(),
        "value = 1234567890\n",
        "the whole fix is skipped"
    );
}

#[test]
fn parse_failure_rolls_the_file_back() {
    let project = tempfile::tempdir().unwrap();
    let sources = load_sources(project.path(), &[("scene.py", "flag = False\n")]);
    let diagnostic = diagnostic_with_fix(
        "scene.py",
        vec![edit("scene.py", span(1, 8, 13), "def def(")],
        FixApplicability::Safe,
    );

    let report = fixes::apply(&sources, &[diagnostic], false).unwrap();
    assert_eq!(report.applied, 0);
    assert!(report.files_changed.is_empty());
    assert_eq!(report.rolled_back.len(), 1);
    assert_eq!(report.rolled_back[0].path, "scene.py");
    assert!(report.rolled_back[0].reason.contains("parse"));
    assert_eq!(
        std::fs::read_to_string(project.path().join("scene.py")).unwrap(),
        "flag = False\n",
        "the file on disk is untouched"
    );
}

/// A miniature stand-in for a fix-emitting rule: it proposes replacing
/// every literal `False` with `True`. The fixtures are ASCII, so byte
/// columns equal character columns.
fn test_rule_fixes(sources: &SourceManager) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    for file in sources.files() {
        for (line_index, line) in file.text().lines().enumerate() {
            if let Some(byte_column) = line.find("False") {
                let column = byte_column + 1;
                diagnostics.push(diagnostic_with_fix(
                    file.relative_path(),
                    vec![edit(
                        file.relative_path(),
                        span(line_index + 1, column, column + "False".len()),
                        "True",
                    )],
                    FixApplicability::Safe,
                ));
            }
        }
    }
    diagnostics
}

#[test]
fn applying_fixes_twice_is_a_no_op() {
    let project = tempfile::tempdir().unwrap();
    let sources = load_sources(
        project.path(),
        &[("scene.py", "a = False\nb = 1\nc = False\n")],
    );

    let first_pass = test_rule_fixes(&sources);
    assert_eq!(first_pass.len(), 2);
    let report = fixes::apply(&sources, &first_pass, false).unwrap();
    assert_eq!(report.applied, 2);
    let after_first = std::fs::read(project.path().join("scene.py")).unwrap();
    assert_eq!(after_first, b"a = True\nb = 1\nc = True\n");

    // Second run: reload the fixed sources, the "rule" finds nothing.
    let mut reloaded = SourceManager::new(project.path());
    reloaded.load_file(&project.path().join("scene.py"));
    let second_pass = test_rule_fixes(&reloaded);
    assert!(second_pass.is_empty(), "nothing left to fix");
    let report = fixes::apply(&reloaded, &second_pass, false).unwrap();
    assert!(report.is_empty());
    assert_eq!(
        std::fs::read(project.path().join("scene.py")).unwrap(),
        after_first,
        "second --fix run leaves the file byte-identical"
    );
}

#[test]
fn unicode_span_edit_on_a_japanese_line() {
    let project = tempfile::tempdir().unwrap();
    let sources = load_sources(
        project.path(),
        &[("scene.py", "label = \"こんにちは世界\"\n")],
    );
    // `label = "` is 9 characters, こんにちは is 5 more: 世界 occupies the
    // one-based character columns 15..17 (end-exclusive).
    let diagnostic = diagnostic_with_fix(
        "scene.py",
        vec![edit("scene.py", span(1, 15, 17), "せかい")],
        FixApplicability::Safe,
    );

    let report = fixes::apply(&sources, &[diagnostic], false).unwrap();
    assert_eq!(report.applied, 1);
    assert_eq!(
        std::fs::read_to_string(project.path().join("scene.py")).unwrap(),
        "label = \"こんにちはせかい\"\n"
    );
}

#[test]
fn check_with_fix_flag_reports_an_empty_pass() {
    // No current rule emits fixes, so `--fix` through the pipeline is an
    // accepted no-op that still produces a report.
    let project = tempfile::tempdir().unwrap();
    write_project(project.path());
    let mut args = args_for(project.path(), OutputFormat::Concise);
    args.fix = true;
    let report = check(&args).unwrap();
    let fix_report = report.fixes.expect("--fix produces a fix report");
    assert!(fix_report.is_empty());
    // The analyzed files are untouched.
    assert_eq!(
        std::fs::read_to_string(project.path().join("scenes/good.py")).unwrap(),
        GOOD_SCENE
    );
}

#[test]
fn missing_baseline_file_is_an_error() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path());
    let mut args = args_for(project.path(), OutputFormat::Concise);
    args.baseline = Some(PathBuf::from("/nonexistent/baseline.json"));
    assert!(check(&args).is_err());
}
