//! Persistent analysis-cache contract tests (DESIGN §9).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

use manim_lint::application::check;
use manim_lint::cache::CacheStatus;
use manim_lint::cli::CheckArgs;
use manim_lint::reporting::OutputFormat;

const CACHE_DATABASE: &str = ".manim-lint-cache/cache-v2.sqlite3";

fn write_project(root: &Path, source: &str) {
    std::fs::write(root.join("pyproject.toml"), "[tool.manim-lint]\n").unwrap();
    std::fs::write(root.join("scene.py"), source).unwrap();
}

fn args_for(root: &Path) -> CheckArgs {
    CheckArgs {
        paths: vec![root.to_path_buf()],
        format: OutputFormat::Json,
        ..CheckArgs::default()
    }
}

fn database(root: &Path) -> PathBuf {
    root.join(CACHE_DATABASE)
}

#[test]
fn identical_second_check_hits_and_source_changes_miss() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path(), "broken = (\n");
    let args = args_for(project.path());

    let cold = check(&args).unwrap();
    assert_eq!(cold.cache_status, CacheStatus::Miss);
    assert!(database(project.path()).is_file());
    assert!(cold.diagnostics.iter().any(|item| item.rule_id == "MLC000"));

    let warm = check(&args).unwrap();
    assert_eq!(warm.cache_status, CacheStatus::Hit);
    assert_eq!(warm.diagnostics, cold.diagnostics);
    assert_eq!(warm.output, cold.output);

    std::fs::write(project.path().join("scene.py"), "value = 1\n").unwrap();
    let changed = check(&args).unwrap();
    assert_eq!(changed.cache_status, CacheStatus::Miss);
    assert!(
        changed
            .diagnostics
            .iter()
            .all(|item| item.rule_id != "MLC000")
    );
    assert_eq!(check(&args).unwrap().cache_status, CacheStatus::Hit);

    std::fs::write(
        project.path().join("pyproject.toml"),
        "[tool.manim-lint]\ntarget-python = \"3.10\"\n",
    )
    .unwrap();
    assert_eq!(check(&args).unwrap().cache_status, CacheStatus::Miss);
}

#[test]
fn no_cache_neither_reads_nor_creates_state() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path(), "broken = (\n");
    let mut args = args_for(project.path());
    args.no_cache = true;

    let first = check(&args).unwrap();
    let second = check(&args).unwrap();
    assert_eq!(first.cache_status, CacheStatus::Disabled);
    assert_eq!(second.cache_status, CacheStatus::Disabled);
    assert_eq!(first.output, second.output);
    assert!(!project.path().join(".manim-lint-cache").exists());
}

#[test]
fn operations_requiring_live_facts_bypass_the_cache() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path(), "value = 1\n");
    let mut args = args_for(project.path());
    args.analysis_summary = true;

    let report = check(&args).unwrap();
    assert_eq!(report.cache_status, CacheStatus::Disabled);
    assert!(report.coverage.is_some());
    assert!(!project.path().join(".manim-lint-cache").exists());
}

#[test]
fn asset_filesystem_dependencies_invalidate_an_entry() {
    let project = tempfile::tempdir().unwrap();
    write_project(
        project.path(),
        "from manim import SVGMobject\nicon = SVGMobject(\"icon.svg\")\n",
    );
    let args = args_for(project.path());

    let missing = check(&args).unwrap();
    assert_eq!(missing.cache_status, CacheStatus::Miss);
    assert!(
        missing
            .diagnostics
            .iter()
            .any(|item| item.rule_id == "MLR104")
    );
    assert_eq!(check(&args).unwrap().cache_status, CacheStatus::Hit);

    std::fs::write(
        project.path().join("icon.svg"),
        "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>\n",
    )
    .unwrap();
    let found = check(&args).unwrap();
    assert_eq!(found.cache_status, CacheStatus::Miss);
    assert!(
        found
            .diagnostics
            .iter()
            .all(|item| item.rule_id != "MLR104")
    );
}

#[test]
fn corrupt_database_is_warned_about_rebuilt_and_reused() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path(), "value = 1\n");
    std::fs::create_dir(project.path().join(".manim-lint-cache")).unwrap();
    std::fs::write(database(project.path()), b"this is not sqlite").unwrap();
    let args = args_for(project.path());

    let rebuilt = check(&args).unwrap();
    assert_eq!(rebuilt.cache_status, CacheStatus::Miss);
    assert!(
        rebuilt
            .cache_warnings
            .iter()
            .any(|warning| warning.contains("corrupt") && warning.contains("rebuilt")),
        "unexpected warnings: {:?}",
        rebuilt.cache_warnings
    );
    assert_eq!(check(&args).unwrap().cache_status, CacheStatus::Hit);

    let connection = rusqlite::Connection::open(database(project.path())).unwrap();
    let journal_mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(journal_mode, "wal");
}

#[test]
fn incompatible_schema_version_is_reinitialized() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path(), "value = 1\n");
    std::fs::create_dir(project.path().join(".manim-lint-cache")).unwrap();
    let connection = rusqlite::Connection::open(database(project.path())).unwrap();
    connection
        .execute_batch("CREATE TABLE analysis_entries (old_key TEXT); PRAGMA user_version = 999;")
        .unwrap();
    drop(connection);

    let args = args_for(project.path());
    let migrated = check(&args).unwrap();
    assert_eq!(migrated.cache_status, CacheStatus::Miss);
    assert!(migrated.cache_warnings.is_empty());
    assert_eq!(check(&args).unwrap().cache_status, CacheStatus::Hit);
}

#[test]
fn corrupt_component_summary_is_rebuilt_without_affecting_results() {
    let project = tempfile::tempdir().unwrap();
    write_project(
        project.path(),
        "from manim import Scene\nclass Demo(Scene):\n    def construct(self):\n        self.play()\n",
    );
    let args = args_for(project.path());
    let cold = check(&args).unwrap();
    assert_eq!(cold.cache_status, CacheStatus::Miss);

    let connection = rusqlite::Connection::open(database(project.path())).unwrap();
    connection
        .execute("DELETE FROM analysis_entries", [])
        .unwrap();
    connection
        .execute("UPDATE component_entries SET summaries_json = '{'", [])
        .unwrap();
    drop(connection);

    let rebuilt = check(&args).unwrap();
    assert_eq!(rebuilt.cache_status, CacheStatus::Miss);
    assert_eq!(rebuilt.output, cold.output);
    assert!(
        rebuilt
            .cache_warnings
            .iter()
            .any(|warning| warning.contains("corrupt") && warning.contains("rebuilt"))
    );
    assert_eq!(check(&args).unwrap().cache_status, CacheStatus::Hit);
}

#[test]
fn out_of_component_nested_summary_site_is_rejected_and_rebuilt() {
    let project = tempfile::tempdir().unwrap();
    write_project(
        project.path(),
        "from manim import Scene\nclass Demo(Scene):\n    def construct(self):\n        self.play()\n",
    );
    let args = args_for(project.path());
    let cold = check(&args).unwrap();
    assert_eq!(cold.cache_status, CacheStatus::Miss);

    let connection = rusqlite::Connection::open(database(project.path())).unwrap();
    let summaries_json: String = connection
        .query_row(
            "SELECT summaries_json FROM component_entries LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut summaries: serde_json::Value = serde_json::from_str(&summaries_json).unwrap();
    assert!(replace_first_nested_site_file(&mut summaries, 99));
    connection
        .execute("DELETE FROM analysis_entries", [])
        .unwrap();
    connection
        .execute(
            "UPDATE component_entries SET summaries_json = ?1",
            [serde_json::to_string(&summaries).unwrap()],
        )
        .unwrap();
    drop(connection);

    let rebuilt = check(&args).unwrap();
    assert_eq!(rebuilt.cache_status, CacheStatus::Miss);
    assert_eq!(rebuilt.output, cold.output);
    assert!(
        rebuilt.cache_warnings.iter().any(|warning| {
            warning.contains("corrupt") && warning.contains("another component")
        }),
        "unexpected warnings: {:?}",
        rebuilt.cache_warnings
    );
    assert_eq!(check(&args).unwrap().cache_status, CacheStatus::Hit);
}

fn replace_first_nested_site_file(value: &mut serde_json::Value, replacement: u64) -> bool {
    match value {
        serde_json::Value::Object(fields) => {
            if fields.contains_key("file")
                && fields.contains_key("start")
                && fields.contains_key("end")
            {
                fields.insert("file".to_owned(), replacement.into());
                return true;
            }
            fields
                .values_mut()
                .any(|value| replace_first_nested_site_file(value, replacement))
        }
        serde_json::Value::Array(values) => values
            .iter_mut()
            .any(|value| replace_first_nested_site_file(value, replacement)),
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => false,
    }
}

#[test]
fn whole_project_entries_are_pruned_while_component_fallback_remains() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path(), "value = 0\n");
    let args = args_for(project.path());

    for revision in 0..16 {
        std::fs::write(
            project.path().join("scene.py"),
            format!("value = {revision}\n"),
        )
        .unwrap();
        assert_eq!(check(&args).unwrap().cache_status, CacheStatus::Miss);
    }

    std::fs::write(project.path().join("scene.py"), "value = 0\n").unwrap();
    assert_eq!(check(&args).unwrap().cache_status, CacheStatus::Hit);
    std::fs::write(project.path().join("scene.py"), "value = 16\n").unwrap();
    assert_eq!(check(&args).unwrap().cache_status, CacheStatus::Miss);

    let connection = rusqlite::Connection::open(database(project.path())).unwrap();
    let entries: i64 = connection
        .query_row("SELECT COUNT(*) FROM analysis_entries", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(entries, 16);
    drop(connection);

    std::fs::write(project.path().join("scene.py"), "value = 0\n").unwrap();
    assert_eq!(check(&args).unwrap().cache_status, CacheStatus::Hit);
    std::fs::write(project.path().join("scene.py"), "value = 1\n").unwrap();
    assert_eq!(check(&args).unwrap().cache_status, CacheStatus::Hit);
}

#[test]
fn component_entries_are_bounded() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path(), "value = 0\n");
    let args = args_for(project.path());

    for revision in 0..260 {
        std::fs::write(
            project.path().join("scene.py"),
            format!("value = {revision}\n"),
        )
        .unwrap();
        assert_eq!(check(&args).unwrap().cache_status, CacheStatus::Miss);
    }
    let connection = rusqlite::Connection::open(database(project.path())).unwrap();
    let entries: i64 = connection
        .query_row("SELECT COUNT(*) FROM component_entries", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(entries, 256);
    drop(connection);

    std::fs::write(project.path().join("scene.py"), "value = 0\n").unwrap();
    assert_eq!(check(&args).unwrap().cache_status, CacheStatus::Miss);
}

#[test]
fn independent_source_change_reuses_unchanged_component() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("pyproject.toml"), "[tool.manim-lint]\n").unwrap();
    std::fs::write(
        project.path().join("a.py"),
        "from manim import Scene\nclass A(Scene):\n    def construct(self):\n        self.play()\n",
    )
    .unwrap();
    std::fs::write(
        project.path().join("b.py"),
        "from manim import Scene\nclass B(Scene):\n    def construct(self):\n        self.wait(0)\n",
    )
    .unwrap();
    let args = args_for(project.path());

    let cold = check(&args).unwrap();
    assert_eq!(cold.cache_status, CacheStatus::Miss);
    assert!(cold.cache_warnings.is_empty());
    assert_eq!(check(&args).unwrap().cache_status, CacheStatus::Hit);

    std::fs::write(
        project.path().join("a.py"),
        "from manim import Scene\nclass A(Scene):\n    def construct(self):\n        pass\n",
    )
    .unwrap();
    let incremental = check(&args).unwrap();
    assert_eq!(incremental.cache_status, CacheStatus::Partial);
    assert!(incremental.cache_warnings.is_empty());

    let mut uncached_args = args_for(project.path());
    uncached_args.no_cache = true;
    let uncached = check(&uncached_args).unwrap();
    assert_eq!(incremental.diagnostics, uncached.diagnostics);
    assert_eq!(incremental.output, uncached.output);
}

#[test]
fn source_layout_change_invalidates_every_component_file_id() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("pyproject.toml"), "[tool.manim-lint]\n").unwrap();
    std::fs::write(project.path().join("a.py"), "value = 1\n").unwrap();
    std::fs::write(project.path().join("b.py"), "value = 2\n").unwrap();
    let args = args_for(project.path());
    assert_eq!(check(&args).unwrap().cache_status, CacheStatus::Miss);

    // FileIds are assigned from the sorted project layout. Adding even an
    // independent path therefore invalidates every component key instead of
    // applying old summaries against shifted numeric handles.
    std::fs::write(project.path().join("00_new.py"), "value = 0\n").unwrap();
    let changed = check(&args).unwrap();
    assert_eq!(changed.cache_status, CacheStatus::Miss);

    let mut uncached_args = args_for(project.path());
    uncached_args.no_cache = true;
    assert_eq!(changed.output, check(&uncached_args).unwrap().output);
}

#[test]
fn changing_shared_helper_invalidates_its_whole_dependency_component() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("pyproject.toml"), "[tool.manim-lint]\n").unwrap();
    std::fs::write(
        project.path().join("shared.py"),
        "def animate(scene):\n    scene.play()\n",
    )
    .unwrap();
    for name in ["a", "b"] {
        std::fs::write(
            project.path().join(format!("{name}.py")),
            format!(
                "from manim import Scene\nfrom shared import animate\nclass {}(Scene):\n    def construct(self):\n        animate(self)\n",
                name.to_uppercase()
            ),
        )
        .unwrap();
    }
    let args = args_for(project.path());
    assert_eq!(check(&args).unwrap().cache_status, CacheStatus::Miss);

    std::fs::write(
        project.path().join("shared.py"),
        "def animate(scene):\n    scene.wait(0)\n",
    )
    .unwrap();
    let incremental = check(&args).unwrap();
    assert_eq!(incremental.cache_status, CacheStatus::Miss);
    assert!(incremental.cache_warnings.is_empty());

    let mut uncached_args = args_for(project.path());
    uncached_args.no_cache = true;
    assert_eq!(
        incremental.output,
        check(&uncached_args).unwrap().output,
        "dependency-component recomputation must match a full analysis"
    );
}

#[test]
fn concurrent_cold_writers_share_the_wal_database() {
    let project = tempfile::tempdir().unwrap();
    write_project(project.path(), "value = 1\n");
    let root = project.path().to_path_buf();
    let barrier = Arc::new(Barrier::new(4));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let root = root.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let args = args_for(&root);
                barrier.wait();
                check(&args).unwrap()
            })
        })
        .collect();
    let reports: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();

    assert!(
        reports
            .iter()
            .all(|report| report.cache_warnings.is_empty())
    );
    assert!(
        reports
            .windows(2)
            .all(|pair| pair[0].output == pair[1].output)
    );
    assert_eq!(
        check(&args_for(&root)).unwrap().cache_status,
        CacheStatus::Hit
    );
}
