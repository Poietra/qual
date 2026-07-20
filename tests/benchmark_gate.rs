//! Cold / warm / incremental benchmark gate over a pinned 10k-LOC corpus
//! (DESIGN §11.4: cold ≤ 2 s, warm ≤ 0.5 s, one-of-20 incremental
//! ≤ 0.5 s, peak RSS < 300 MiB on the reference machine in
//! `benchmarks/reference-machine.json`).
//!
//! The fixture under `tests/corpus/benchmark_10kloc/` is committed AND
//! reproducible: `generated_files()` below is a deterministic generator
//! (fixed-seed LCG, no filesystem or clock input), and the non-ignored
//! test asserts the committed files are byte-identical to its output, so
//! the benchmark input can never drift silently.
//!
//! Run the gates:
//!
//! ```sh
//! cargo test --release --test benchmark_gate -- --ignored benchmark
//! cargo test --test benchmark_gate -- --ignored regenerate  # rebuild fixture
//! ```
//!
//! The timing test asserts thresholds only when the current machine
//! matches the reference fingerprint (CPU model + OS); elsewhere it
//! prints the measurements informationally. Peak RSS is read from
//! `/proc/self/status` `VmHWM` (Linux only, informational elsewhere).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use manim_lint::application::check;
use manim_lint::cache::CacheStatus;
use manim_lint::cli::CheckArgs;
use manim_lint::reporting::OutputFormat;

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/benchmark_10kloc")
}

// ---------------------------------------------------------------------------
// Deterministic fixture generator
// ---------------------------------------------------------------------------

/// Deterministic linear congruential generator (Numerical Recipes
/// constants). No global state, no clock, no platform dependence.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn pick<'a>(&mut self, options: &[&'a str]) -> &'a str {
        options[usize::try_from(self.next()).unwrap_or(0) % options.len()]
    }

    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }
}

const SHAPES: &[&str] = &["Square", "Circle", "Triangle", "Dot", "Star", "Rectangle"];
const COLORS: &[&str] = &["RED", "BLUE", "GREEN", "YELLOW", "PURPLE", "ORANGE"];
const DIRECTIONS: &[&str] = &["LEFT", "RIGHT", "UP", "DOWN"];
const INTRODUCERS: &[&str] = &["Create", "FadeIn", "GrowFromCenter", "DrawBorderThenFill"];
const REMOVERS: &[&str] = &["FadeOut", "Uncreate"];

/// One Manim-realistic scene class, ~24 lines, deterministic in `rng`.
fn scene_class(rng: &mut Lcg, file_index: usize, scene_index: usize) -> String {
    let mut body = String::new();
    let _ = writeln!(body, "class Scene{file_index:02}_{scene_index:02}(Scene):");
    let _ = writeln!(body, "    def construct(self):");
    let shape_count = rng.range(3, 4);
    let mut names: Vec<String> = Vec::new();
    for shape_index in 0..shape_count {
        let shape = rng.pick(SHAPES);
        let color = rng.pick(COLORS);
        let name = format!("mob{shape_index}");
        let _ = writeln!(body, "        {name} = {shape}(color={color})");
        let _ = writeln!(
            body,
            "        {name}.shift({direction} * {offset})",
            direction = rng.pick(DIRECTIONS),
            offset = rng.range(1, 3),
        );
        names.push(name);
    }
    let _ = writeln!(
        body,
        "        title = Text(\"scene {file_index:02}-{scene_index:02} step {step}\")",
        step = rng.range(1, 99),
    );
    let _ = writeln!(body, "        title.to_edge(UP)");
    let _ = writeln!(
        body,
        "        formula = MathTex(r\"\\sum_{{k=1}}^{{{upper}}} k^{power}\")",
        upper = rng.range(3, 20),
        power = rng.range(2, 4),
    );
    let _ = writeln!(body, "        formula.next_to(title, DOWN)");
    let _ = writeln!(
        body,
        "        group = VGroup({names})",
        names = names.join(", "),
    );
    let _ = writeln!(body, "        self.add(title, formula)");
    let _ = writeln!(
        body,
        "        self.play({introducer}(group), run_time={run_time})",
        introducer = rng.pick(INTRODUCERS),
        run_time = rng.range(1, 3),
    );
    for name in &names {
        let _ = writeln!(
            body,
            "        self.play({name}.animate.shift({direction} * {offset}), run_time={run_time})",
            direction = rng.pick(DIRECTIONS),
            offset = rng.range(1, 2),
            run_time = rng.range(1, 2),
        );
    }
    let _ = writeln!(
        body,
        "        self.play(Transform({a}, {b}), run_time={run_time})",
        a = names[0],
        b = names[1],
        run_time = rng.range(1, 2),
    );
    let _ = writeln!(
        body,
        "        self.wait({seconds})",
        seconds = rng.range(1, 2)
    );
    let _ = writeln!(
        body,
        "        self.play({remover}(group), FadeOut(title), FadeOut(formula))",
        remover = rng.pick(REMOVERS),
    );
    let _ = writeln!(body, "        self.wait(1)");
    body
}

/// The full deterministic fixture: file name → file content. About
/// 10,400 physical lines of Manim-realistic scene code across 20 files.
fn generated_files() -> Vec<(String, String)> {
    let mut rng = Lcg(0x006d_616e_696d_6c74); // fixed seed, never change
    let mut files = Vec::new();
    for file_index in 0..20 {
        let mut content = String::new();
        let _ = writeln!(content, "\"\"\"Benchmark fixture file {file_index:02}.");
        content.push('\n');
        let _ = writeln!(
            content,
            "Deterministically generated by tests/benchmark_gate.rs; do not edit."
        );
        let _ = writeln!(content, "\"\"\"");
        let _ = writeln!(content, "from manim import *");
        for scene_index in 0..21 {
            let _ = writeln!(content, "\n");
            content.push_str(&scene_class(&mut rng, file_index, scene_index));
        }
        files.push((format!("scenes_{file_index:02}.py"), content));
    }
    files
}

fn total_lines(files: &[(String, String)]) -> usize {
    files
        .iter()
        .map(|(_, content)| content.lines().count())
        .sum()
}

// ---------------------------------------------------------------------------
// Fixture integrity (normal tests)
// ---------------------------------------------------------------------------

/// The committed fixture is byte-identical to the deterministic generator
/// and reaches the DESIGN §11.4 size (≥ 10,000 LOC).
#[test]
fn benchmark_fixture_matches_generator() {
    let root = fixture_root();
    let files = generated_files();
    assert!(
        total_lines(&files) >= 10_000,
        "the benchmark fixture must be at least 10k LOC, generator yields {}",
        total_lines(&files)
    );
    for (name, expected) in &files {
        let path = root.join(name);
        let committed = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "missing benchmark fixture file {name}: {error}\n\
                 regenerate with: cargo test --test benchmark_gate -- --ignored regenerate"
            )
        });
        assert!(
            &committed == expected,
            "benchmark fixture file {name} differs from the deterministic \
             generator output; the benchmark input must never drift. \
             Regenerate with: cargo test --test benchmark_gate -- --ignored regenerate"
        );
    }
    // No stray files: the fixture directory is exactly the generator output.
    let mut on_disk: Vec<String> = std::fs::read_dir(&root)
        .expect("fixture directory")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    on_disk.sort();
    let mut expected_names: Vec<String> = files.iter().map(|(name, _)| name.clone()).collect();
    expected_names.sort();
    assert_eq!(
        on_disk, expected_names,
        "unexpected files in tests/corpus/benchmark_10kloc"
    );
}

/// Rebuilds `tests/corpus/benchmark_10kloc/` from the generator. Ignored:
/// run explicitly after intentionally changing the generator, then commit
/// the result.
#[test]
#[ignore = "writes the committed fixture; run explicitly to regenerate"]
fn regenerate() {
    let root = fixture_root();
    std::fs::create_dir_all(&root).expect("create fixture directory");
    for (name, content) in generated_files() {
        std::fs::write(root.join(&name), content).expect("write fixture file");
    }
}

// ---------------------------------------------------------------------------
// Timing gate (ignored: run explicitly, in release, on a quiet machine)
// ---------------------------------------------------------------------------

/// `VmHWM` (peak resident set) in MiB from `/proc/self/status`; `None`
/// off Linux.
fn peak_rss_mib() -> Option<f64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
    let kib: f64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib / 1024.0)
}

/// `cpu model + OS pretty name` fingerprint of the current machine;
/// `None` off Linux.
fn machine_fingerprint() -> Option<(String, String)> {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    let cpu = cpuinfo
        .lines()
        .find(|line| line.starts_with("model name"))?
        .split(':')
        .nth(1)?
        .trim()
        .to_owned();
    let os_release = std::fs::read_to_string("/etc/os-release").ok()?;
    let os = os_release
        .lines()
        .find(|line| line.starts_with("PRETTY_NAME="))?
        .trim_start_matches("PRETTY_NAME=")
        .trim_matches('"')
        .to_owned();
    Some((cpu, os))
}

fn run_check_over_fixture(root: &Path) -> (usize, CacheStatus) {
    let args = CheckArgs {
        paths: vec![root.to_path_buf()],
        format: OutputFormat::Json,
        ..CheckArgs::default()
    };
    let report = check(&args).expect("check pipeline over benchmark fixture");
    (report.diagnostics.len(), report.cache_status)
}

fn isolated_fixture() -> tempfile::TempDir {
    let project = tempfile::tempdir().expect("temporary benchmark project");
    std::fs::write(project.path().join("pyproject.toml"), "[tool.manim-lint]\n")
        .expect("benchmark pyproject");
    for (name, content) in generated_files() {
        std::fs::write(project.path().join(name), content).expect("benchmark source file");
    }
    project
}

/// DESIGN §11.4 benchmark gate. `cold` starts with no cache database; `warm`
/// is the immediately following identical check; `incremental` changes one
/// of twenty independent source components and must reuse the other nineteen.
#[test]
#[ignore = "wall-clock benchmark; run in --release on a quiet machine"]
#[allow(
    clippy::too_many_lines,
    reason = "one linear cold/warm/incremental measurement and assertion pipeline"
)]
fn benchmark_cold_warm_and_incremental_within_reference_budget() {
    // Fail fast if the input drifted: timings over a different corpus
    // would be meaningless.
    benchmark_fixture_matches_generator();
    let project = isolated_fixture();
    let cache_database = project.path().join(".manim-lint-cache/cache-v2.sqlite3");
    assert!(
        !cache_database.exists(),
        "cold run must start without a cache"
    );

    let cold_start = Instant::now();
    let (diagnostics_cold, cold_status) = run_check_over_fixture(project.path());
    let cold = cold_start.elapsed().as_secs_f64();
    assert_eq!(cold_status, CacheStatus::Miss);
    assert!(cache_database.is_file(), "cold run must populate the cache");

    let warm_start = Instant::now();
    let (diagnostics_warm, warm_status) = run_check_over_fixture(project.path());
    let warm = warm_start.elapsed().as_secs_f64();
    assert_eq!(warm_status, CacheStatus::Hit);

    let changed_path = project.path().join("scenes_00.py");
    let mut changed = std::fs::read_to_string(&changed_path).expect("read changed component");
    changed.push_str("\n# incremental benchmark edit\n");
    std::fs::write(&changed_path, changed).expect("write changed component");
    let incremental_start = Instant::now();
    let (diagnostics_incremental, incremental_status) = run_check_over_fixture(project.path());
    let incremental = incremental_start.elapsed().as_secs_f64();
    assert_eq!(incremental_status, CacheStatus::Partial);

    assert_eq!(
        diagnostics_cold, diagnostics_warm,
        "cold and warm runs must produce identical results"
    );
    assert_eq!(
        diagnostics_cold, diagnostics_incremental,
        "a semantics-neutral incremental edit must preserve results"
    );

    let rss = peak_rss_mib();
    println!("benchmark_10kloc: {diagnostics_cold} diagnostics");
    println!("cold: {cold:.3} s (budget 2.0 s)");
    println!("warm: {warm:.3} s (budget 0.5 s)");
    println!("incremental 1/20: {incremental:.3} s (budget 0.5 s)");
    match rss {
        Some(mib) => println!("peak RSS (VmHWM): {mib:.1} MiB (budget 300 MiB)"),
        None => println!("peak RSS: unavailable on this platform (informational)"),
    }

    let reference_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("benchmarks/reference-machine.json");
    let reference: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&reference_path).expect("read benchmarks/reference-machine.json"),
    )
    .expect("parse benchmarks/reference-machine.json");

    let matches_reference = machine_fingerprint().is_some_and(|(cpu, os)| {
        reference["fingerprint"]["cpu_model"] == cpu.as_str()
            && reference["fingerprint"]["os"] == os.as_str()
    });

    if !matches_reference {
        println!(
            "current machine does not match the reference fingerprint in \
             {path}; measurements above are informational only",
            path = reference_path.display()
        );
        return;
    }

    let cold_budget = reference["thresholds"]["cold_seconds"]
        .as_f64()
        .expect("thresholds.cold_seconds");
    let warm_budget = reference["thresholds"]["warm_seconds"]
        .as_f64()
        .expect("thresholds.warm_seconds");
    let incremental_budget = reference["thresholds"]["incremental_seconds"]
        .as_f64()
        .expect("thresholds.incremental_seconds");
    let rss_budget = reference["thresholds"]["peak_rss_mib"]
        .as_f64()
        .expect("thresholds.peak_rss_mib");
    assert!(
        cold <= cold_budget,
        "cold 10k-LOC check took {cold:.3} s, budget {cold_budget} s"
    );
    if reference["enforced"]["warm_seconds"] == true {
        assert!(
            warm <= warm_budget,
            "warm 10k-LOC check took {warm:.3} s, budget {warm_budget} s"
        );
    } else {
        println!(
            "warm budget ({warm_budget} s) is disabled in \
             benchmarks/reference-machine.json"
        );
    }
    if reference["enforced"]["incremental_seconds"] == true {
        assert!(
            incremental <= incremental_budget,
            "incremental 1/20 check took {incremental:.3} s, budget {incremental_budget} s"
        );
    } else {
        println!(
            "incremental budget ({incremental_budget} s) is disabled in \
             benchmarks/reference-machine.json"
        );
    }
    if let Some(mib) = rss {
        assert!(
            mib < rss_budget,
            "peak RSS {mib:.1} MiB, budget {rss_budget} MiB"
        );
    }
}
