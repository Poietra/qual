//! Phase 3 performance rule golden tests, fork-overlay tranche
//! (DESIGN §11.1, §7.3): `MLP214`, `MLP217`.
//!
//! Both rules read the curated `fork_capabilities` block of the selected
//! knowledge profile, so each fixture directory carries a `pyproject.toml`
//! selecting `local_0_20_1_4d25c031`. The inertness tests rerun the same
//! fixtures under `upstream_0_20` (whose accessors return `None`) and
//! require that no fork-gated diagnostic survives (DESIGN §7.3: never
//! suggest an API the selected profile does not have).

use std::path::{Path, PathBuf};

use manim_lint::application::check;
use manim_lint::cli::CheckArgs;
use manim_lint::diagnostic::{Confidence, Severity};
use manim_lint::reporting::OutputFormat;
use manim_lint::rules::registry;

fn fixture_dir(rule: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/rules")
        .join(rule)
}

/// Copies every fixture file of one rule (its fork-profile
/// `pyproject.toml` included) into a fresh temp project.
fn copy_fixture(rule: &str) -> tempfile::TempDir {
    let project = tempfile::tempdir().unwrap();
    for entry in std::fs::read_dir(fixture_dir(rule)).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            std::fs::copy(&path, project.path().join(path.file_name().unwrap())).unwrap();
        }
    }
    project
}

fn args_for(root: &Path) -> CheckArgs {
    CheckArgs {
        paths: vec![root.to_path_buf()],
        format: OutputFormat::Json,
        ..CheckArgs::default()
    }
}

/// Runs `check` over a rule fixture directory and renders each diagnostic
/// as `path:line:column rule severity confidence`, in pipeline order.
fn observed(root: &Path) -> Vec<String> {
    observed_with(&args_for(root))
}

fn observed_with(args: &CheckArgs) -> Vec<String> {
    let report = check(args).unwrap();
    for diagnostic in &report.diagnostics {
        assert!(
            !diagnostic.evidence.is_empty(),
            "{}: evidence must be machine-readable and non-empty",
            diagnostic.rule_id
        );
        assert!(
            diagnostic.explanation.is_some(),
            "{}: explanation must state the Manim semantics",
            diagnostic.rule_id
        );
    }
    report
        .diagnostics
        .iter()
        .map(|diagnostic| {
            format!(
                "{path}:{line}:{column} {rule} {severity} {confidence}",
                path = diagnostic.path,
                line = diagnostic.primary_span.start.line,
                column = diagnostic.primary_span.start.column,
                rule = diagnostic.rule_id,
                severity = diagnostic.severity,
                confidence = diagnostic.confidence,
            )
        })
        .collect()
}

fn assert_golden(rule: &str, expected: &[&str]) {
    let project = copy_fixture(rule);
    let rows = observed(project.path());
    assert_eq!(rows, expected, "golden diagnostics for {rule}");
    assert!(
        !rows.iter().any(|row| row.contains("suppressed.py")),
        "{rule}: inline suppression must win"
    );
}

/// Reruns a fixture under `upstream_0_20` and returns the rows.
fn observed_under_upstream(rule: &str) -> Vec<String> {
    let project = copy_fixture(rule);
    std::fs::write(
        project.path().join("pyproject.toml"),
        "[tool.manim-lint]\nknowledge-profile = \"upstream_0_20\"\n",
    )
    .unwrap();
    observed(project.path())
}

#[test]
fn mlp214_serial_tex_compile_keys_golden() {
    // `invalid.py` builds five TeX mobjects before its first play, four
    // distinct literal compile keys among them (the repeated integral is
    // ONE compile job): one diagnostic, anchored at the first key, with
    // the other three as related locations. `valid.py` pins the emission
    // gates: three distinct keys stay below the threshold, eight
    // duplicate constructions are two jobs, keys built after the first
    // play are not pre-play cold jobs (warm/ordering silence), f-string
    // keys are never literal-provable, and a `tex_template` keyword
    // leaves the key unproven. `branch.py` (constructions on one path
    // only) proves no cold compile.
    assert_golden("MLP214", &["invalid.py:6:17 MLP214 info high"]);
}

#[test]
fn mlp217_frame_varying_svg_cache_golden() {
    // `invalid.py`: the frame-varying `SVGMobject` key uses the fork's
    // `use_svg_cache=True` default; the frame-varying `Text` key opts in
    // explicitly (the fork's `Text` defaults to `False`). MLP217 reports
    // the process-global cache growth on both. The DESIGN §7.3 supersedes
    // table orders neither MLP217 nor MLP226/MLP201 above the other —
    // they claim different defects — so the SVG span also carries MLP201
    // (per-frame construction) and the Text span also carries MLP226
    // (per-frame key/disk churn). `valid.py` (default-off `Text`,
    // explicit `use_svg_cache=False`, static key) and `branch.py`
    // (non-literal flag, `**kwargs` splat) never prove the flag on, so
    // MLP217 stays silent there while the generic hot-construction rules
    // still apply. `valid.py`'s `SuspendedHost` (its own `FadeIn` suspends
    // the updater and no later play runs it) is the liveness near miss:
    // no rule may claim per-frame execution there.
    assert_golden(
        "MLP217",
        &[
            "branch.py:10:21 MLP201 warning high",
            "branch.py:17:21 MLP201 warning high",
            "invalid.py:7:39 MLP201 warning high",
            "invalid.py:7:39 MLP217 warning high",
            "invalid.py:9:21 MLP217 warning high",
            "invalid.py:9:21 MLP226 warning high",
            "valid.py:8:39 MLP226 warning high",
            "valid.py:11:21 MLP201 warning high",
            "valid.py:14:40 MLP201 warning high",
        ],
    );
}

/// Under `upstream_0_20` the fork-gated rules are fully inert: the same
/// fixtures produce not a single MLP214 row (the submit-all API does not
/// exist there to be suggested).
#[test]
fn mlp214_is_inert_under_the_upstream_profile() {
    assert_eq!(observed_under_upstream("MLP214"), Vec::<String>::new());
}

/// Under `upstream_0_20` MLP217 vanishes while the profile-independent
/// diagnostics on the same spans (MLP201 / MLP226) survive unchanged: the
/// gate removes exactly the fork-declared interpretation, nothing else.
#[test]
fn mlp217_is_inert_under_the_upstream_profile() {
    let rows = observed_under_upstream("MLP217");
    assert!(
        !rows.iter().any(|row| row.contains("MLP217")),
        "MLP217 must be inert under upstream_0_20: {rows:?}"
    );
    assert_eq!(
        rows,
        &[
            "branch.py:10:21 MLP201 warning high",
            "branch.py:17:21 MLP201 warning high",
            "invalid.py:7:39 MLP201 warning high",
            "invalid.py:9:21 MLP226 warning high",
            "valid.py:8:39 MLP226 warning high",
            "valid.py:11:21 MLP201 warning high",
            "valid.py:14:40 MLP201 warning high",
        ]
    );
}

/// `--select MLP214` still computes the lifecycle facts the rule declares
/// (capability gating, DESIGN §6.3).
#[test]
fn mlp214_still_fires_under_a_narrow_select() {
    let project = copy_fixture("MLP214");
    let mut args = args_for(project.path());
    args.select = vec!["MLP214".to_owned()];
    assert_eq!(
        observed_with(&args),
        vec!["invalid.py:6:17 MLP214 info high"]
    );
}

/// `--select MLP217` still computes the cost facts the rule declares.
#[test]
fn mlp217_still_fires_under_a_narrow_select() {
    let project = copy_fixture("MLP217");
    let mut args = args_for(project.path());
    args.select = vec!["MLP217".to_owned()];
    assert_eq!(
        observed_with(&args),
        vec![
            "invalid.py:7:39 MLP217 warning high",
            "invalid.py:9:21 MLP217 warning high",
        ]
    );
}

/// MLP214's evidence must cite the fork's curated entry points and the
/// literal compile keys it counted (DESIGN §7.3: advice names the real
/// curated API; every count is auditable).
#[test]
fn mlp214_evidence_cites_the_curated_entry_points() {
    let project = copy_fixture("MLP214");
    let report = check(&args_for(project.path())).unwrap();
    let diagnostic = report
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.rule_id == "MLP214")
        .expect("MLP214 must fire on the fixture");
    let entry_points: Vec<String> = diagnostic.evidence["entry_points"]
        .as_array()
        .expect("entry_points evidence")
        .iter()
        .map(|value| value.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        entry_points,
        [
            "manim.mobject.text.tex_mobject.MathTex.precompile",
            "manim.utils.tex_file_writing.tex_to_svg_file_async",
        ]
    );
    assert_eq!(diagnostic.evidence["distinct_compile_keys"], 4);
    assert_eq!(
        diagnostic.evidence["compile_keys"]
            .as_array()
            .expect("compile_keys evidence")
            .len(),
        4,
        "the duplicate formula must not be double counted"
    );
    assert_eq!(
        diagnostic.related_locations.len(),
        3,
        "the other three distinct keys are related locations"
    );
    // The in-flight-TeX / Cairo-fork interaction is explanation text.
    assert!(
        diagnostic
            .explanation
            .as_deref()
            .unwrap()
            .contains("serial fallback"),
        "the serial-fallback interaction must be explained"
    );
}

/// MLP217's evidence must carry the declared cache semantics it relies on.
#[test]
fn mlp217_evidence_carries_the_declared_cache_semantics() {
    let project = copy_fixture("MLP217");
    let report = check(&args_for(project.path())).unwrap();
    let diagnostic = report
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.rule_id == "MLP217")
        .expect("MLP217 must fire on the fixture");
    assert!(
        diagnostic.evidence.contains_key("cache_keyed_by"),
        "the curated key components must be cited"
    );
    assert!(
        diagnostic.evidence["memory_growth"]
            .as_str()
            .unwrap()
            .contains("O(F × family)"),
        "the growth claim is the rule's cost evidence"
    );
    assert!(
        diagnostic.evidence.contains_key("execution"),
        "the liveness plays must be auditable"
    );
}

/// Metadata matches the DESIGN §7.3 catalog rows, and both rules declare
/// the fork-overlay capability besides their fact layers.
#[test]
fn fork_tranche_metadata_matches_the_design_catalog() {
    let expected: [(&str, Severity, Confidence, &[&str]); 2] = [
        ("MLP214", Severity::Info, Confidence::High, &["lifecycle"]),
        (
            "MLP217",
            Severity::Warning,
            Confidence::High,
            &["cost-facts"],
        ),
    ];
    for (rule, severity, confidence, layers) in expected {
        let metadata = registry::metadata_for(rule)
            .unwrap_or_else(|| panic!("{rule} must be registered as implemented"));
        assert!(metadata.default_enabled, "{rule} defaults to enabled");
        assert_eq!(metadata.default_severity, severity, "{rule} severity");
        assert_eq!(metadata.minimum_confidence, confidence, "{rule} confidence");
        assert_eq!(metadata.implementation_phase, 3, "{rule} phase");
        assert_eq!(metadata.supersedes, &[] as &[&str], "{rule} supersedes");
        assert!(
            metadata
                .required_capabilities
                .contains(&"local-fork-overlay"),
            "{rule} must declare the fork-overlay capability"
        );
        for layer in layers {
            assert!(
                metadata.required_capabilities.contains(layer),
                "{rule} must declare the {layer} fact layer"
            );
        }
    }
}
