//! Command orchestration (DESIGN §8.1): resolve config, collect files,
//! parse, run rules, apply suppressions, filter, sort, render.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::cli::{CheckArgs, Command, ExitStatus};
use crate::config::loader::{self, ProfileSelection, ResolutionInput};
use crate::config::model::{ConfigFragment, ResolvedConfig};
use crate::diagnostic::Diagnostic;
use crate::reporting::{self, RenderContext, suppressions};
use crate::rules::RuleContext;
use crate::rules::registry;
use crate::source::SourceManager;

/// Errors that abort a command; all map to exit code 2.
#[derive(Debug, thiserror::Error)]
pub enum ApplicationError {
    /// Configuration discovery or validation failed.
    #[error(transparent)]
    Config(#[from] loader::ConfigError),
    /// A CLI-level input is invalid (e.g. a nonexistent path).
    #[error("{0}")]
    Cli(String),
    /// The requested feature belongs to a later phase.
    #[error("{0}")]
    Unimplemented(String),
    /// Unexpected IO failure outside per-file MLC000 handling.
    #[error("io error on {path}: {source}")]
    Io {
        /// Path involved in the failure.
        path: PathBuf,
        /// Underlying IO error.
        source: std::io::Error,
    },
}

/// Result of executing one CLI command.
#[derive(Debug)]
pub struct Execution {
    /// Text for stdout (may be empty).
    pub stdout: String,
    /// Optional text for stderr (e.g. `--statistics`).
    pub stderr: Option<String>,
    /// Process exit status.
    pub exit: ExitStatus,
}

impl Execution {
    fn success(stdout: String) -> Self {
        Self {
            stdout,
            stderr: None,
            exit: ExitStatus::Success,
        }
    }
}

/// Executes a parsed CLI command.
pub fn execute(command: Command) -> Result<Execution, ApplicationError> {
    match command {
        Command::Check(args) => run_check(&args),
        Command::Explain { rule } => run_explain(&rule),
        Command::Rules => Ok(Execution::success(render_rules())),
        Command::Config => run_config(),
        Command::Cost { .. } => Err(ApplicationError::Unimplemented(
            "`manim-lint cost` is not implemented until Phase 3".to_owned(),
        )),
    }
}

/// Outcome of a `check` run, for library callers and tests.
#[derive(Debug)]
pub struct CheckReport {
    /// Final (filtered, suppressed, sorted) diagnostics.
    pub diagnostics: Vec<Diagnostic>,
    /// Rendered output in the requested format.
    pub output: String,
    /// Exit status implied by `fail-level` and `min-confidence`.
    pub exit: ExitStatus,
    /// The resolved configuration used for the run.
    pub config: ResolvedConfig,
}

/// Runs `manim-lint check` and renders its output.
pub fn run_check(args: &CheckArgs) -> Result<Execution, ApplicationError> {
    reject_unimplemented_flags(args)?;
    let report = check(args)?;
    let stderr = args
        .statistics
        .then(|| render_statistics(&report.diagnostics));
    Ok(Execution {
        stdout: report.output,
        stderr,
        exit: report.exit,
    })
}

/// Full `check` pipeline as a library entry point (used by tests).
pub fn check(args: &CheckArgs) -> Result<CheckReport, ApplicationError> {
    let paths = normalized_input_paths(args)?;
    let project_root = discover_project_root(&paths)?;
    let config = resolve_config(args, &project_root)?;

    let files = collect_python_files(&paths, &project_root, &config.exclude)?;
    let mut sources = SourceManager::new(project_root.clone());
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut suppression_indexes = BTreeMap::new();

    for path in &files {
        let id = sources.load_file(path);
        let file = sources.file(id);
        if let Some(diagnostic) = file.parse_diagnostic() {
            diagnostics.push(diagnostic.clone());
        }
        let (index, warnings) = suppressions::collect(file);
        diagnostics.extend(warnings);
        if !index.is_empty() {
            suppression_indexes.insert(file.relative_path().to_owned(), index);
        }
    }

    let context = RuleContext::new(&sources, &config);
    for rule in registry::all_rules() {
        diagnostics.extend(rule.run(&context));
    }

    diagnostics.retain(|diagnostic| {
        suppression_indexes
            .get(&diagnostic.path)
            .is_none_or(|index| !index.suppresses(diagnostic))
    });

    let per_file_ignores = build_per_file_ignores(&config)?;
    diagnostics.retain(|diagnostic| {
        is_selected(diagnostic, &config.select, &config.ignore)
            && diagnostic.confidence.reaches(config.min_confidence)
            && !per_file_ignores.iter().any(|(globs, selectors)| {
                globs.is_match(&diagnostic.path)
                    && selectors.iter().any(|selector| {
                        suppressions::selector_matches(selector, &diagnostic.rule_id)
                    })
            })
    });

    diagnostics.sort_by(Diagnostic::compare_stable);

    let profiles = config.active_profile_names();
    let render_context = RenderContext {
        tool_version: crate::VERSION,
        project_root: ".",
        profiles: &profiles,
    };
    let output = reporting::render(args.format, &diagnostics, &render_context)
        .map_err(ApplicationError::Unimplemented)?;

    let exit = if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity.reaches(config.fail_level))
    {
        ExitStatus::Failure
    } else {
        ExitStatus::Success
    };

    Ok(CheckReport {
        diagnostics,
        output,
        exit,
        config,
    })
}

fn reject_unimplemented_flags(args: &CheckArgs) -> Result<(), ApplicationError> {
    if args.baseline.is_some() {
        return Err(ApplicationError::Unimplemented(
            "--baseline is not implemented until Phase 5".to_owned(),
        ));
    }
    if args.write_baseline.is_some() {
        return Err(ApplicationError::Unimplemented(
            "--write-baseline is not implemented until Phase 5".to_owned(),
        ));
    }
    if args.unsafe_fixes && !args.fix {
        return Err(ApplicationError::Cli(
            "--unsafe-fixes requires --fix".to_owned(),
        ));
    }
    // --fix and --no-cache are accepted no-ops: no Phase 0 rule produces
    // fixes and no cache exists yet.
    Ok(())
}

fn normalized_input_paths(args: &CheckArgs) -> Result<Vec<PathBuf>, ApplicationError> {
    let raw = if args.paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        args.paths.clone()
    };
    let mut paths = Vec::new();
    for path in raw {
        let canonical = path.canonicalize().map_err(|_| {
            ApplicationError::Cli(format!("path does not exist: {}", path.display()))
        })?;
        paths.push(canonical);
    }
    Ok(paths)
}

/// Project root: the directory holding the nearest `pyproject.toml` above the
/// first input path, or that path's directory when none exists.
fn discover_project_root(paths: &[PathBuf]) -> Result<PathBuf, ApplicationError> {
    let first = paths
        .first()
        .ok_or_else(|| ApplicationError::Cli("no input paths".to_owned()))?;
    let base = if first.is_dir() {
        first.clone()
    } else {
        first
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    };
    Ok(loader::find_pyproject(&base)
        .and_then(|pyproject| pyproject.parent().map(Path::to_path_buf))
        .unwrap_or(base))
}

fn resolve_config(
    args: &CheckArgs,
    project_root: &Path,
) -> Result<ResolvedConfig, ApplicationError> {
    let pyproject_path = project_root.join("pyproject.toml");
    let pyproject = if pyproject_path.is_file() {
        loader::load_pyproject(&pyproject_path)?
    } else {
        None
    };
    let manim_cfg = loader::load_manim_cfg(project_root)?;

    let cli = ConfigFragment {
        select: (!args.select.is_empty()).then(|| args.select.clone()),
        ignore: (!args.ignore.is_empty()).then(|| args.ignore.clone()),
        min_confidence: args.min_confidence,
        fail_level: args.fail_level,
        renderer: args.renderer,
        frame_rate: args.fps,
        pixel_width: args.resolution.map(|resolution| resolution.width),
        pixel_height: args.resolution.map(|resolution| resolution.height),
        ..ConfigFragment::default()
    };

    let input = ResolutionInput {
        project_root: project_root.to_path_buf(),
        cli,
        pyproject,
        manim_cfg,
        profile_selection: ProfileSelection::from_cli(args.profile.as_deref()),
    };
    Ok(loader::resolve(&input)?)
}

/// Collects `.py` files under the input paths, deterministic order,
/// honoring the resolved exclusion globs.
fn collect_python_files(
    paths: &[PathBuf],
    project_root: &Path,
    exclude: &[String],
) -> Result<Vec<PathBuf>, ApplicationError> {
    let exclude_files = build_globset(exclude)?;
    // Also skip whole directories named by `dir/**` patterns.
    let dir_patterns: Vec<String> = exclude
        .iter()
        .map(|pattern| pattern.trim_end_matches("/**").to_owned())
        .collect();
    let exclude_dirs = build_globset(&dir_patterns)?;

    let mut files = Vec::new();
    for path in paths {
        if path.is_file() {
            files.push(path.clone());
        } else {
            walk_directory(
                path,
                project_root,
                &exclude_files,
                &exclude_dirs,
                &mut files,
            )?;
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn walk_directory(
    dir: &Path,
    project_root: &Path,
    exclude_files: &GlobSet,
    exclude_dirs: &GlobSet,
    files: &mut Vec<PathBuf>,
) -> Result<(), ApplicationError> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|source| ApplicationError::Io {
            path: dir.to_path_buf(),
            source,
        })?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    entries.sort();

    for entry in entries {
        let relative = crate::source::relative_posix_path(project_root, &entry);
        if entry.is_dir() {
            let name = entry
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if name == ".git" || name == "__pycache__" || exclude_dirs.is_match(&relative) {
                continue;
            }
            walk_directory(&entry, project_root, exclude_files, exclude_dirs, files)?;
        } else if entry.extension().is_some_and(|extension| extension == "py")
            && !exclude_files.is_match(&relative)
        {
            files.push(entry);
        }
    }
    Ok(())
}

fn build_globset(patterns: &[String]) -> Result<GlobSet, ApplicationError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = Glob::new(pattern)
            .map_err(|error| ApplicationError::Cli(format!("invalid exclude glob: {error}")))?;
        builder.add(glob);
    }
    builder
        .build()
        .map_err(|error| ApplicationError::Cli(format!("invalid exclude globs: {error}")))
}

fn build_per_file_ignores(
    config: &ResolvedConfig,
) -> Result<Vec<(GlobSet, Vec<String>)>, ApplicationError> {
    let mut result = Vec::new();
    for (pattern, selectors) in &config.per_file_ignores {
        let globs = build_globset(std::slice::from_ref(pattern))?;
        result.push((globs, selectors.clone()));
    }
    Ok(result)
}

fn is_selected(diagnostic: &Diagnostic, select: &[String], ignore: &[String]) -> bool {
    let selected = select
        .iter()
        .any(|selector| suppressions::selector_matches(selector, &diagnostic.rule_id));
    let ignored = ignore
        .iter()
        .any(|selector| suppressions::selector_matches(selector, &diagnostic.rule_id));
    selected && !ignored
}

fn render_statistics(diagnostics: &[Diagnostic]) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for diagnostic in diagnostics {
        *counts.entry(diagnostic.rule_id.as_str()).or_default() += 1;
    }
    let mut output = String::new();
    for (rule_id, count) in &counts {
        let _ = writeln!(output, "{count:>6}  {rule_id}");
    }
    let _ = writeln!(output, "{:>6}  total", diagnostics.len());
    output
}

fn run_explain(rule: &str) -> Result<Execution, ApplicationError> {
    /// Embedded rule documentation for implemented rules.
    const DOCS: [(&str, &str); 2] = [
        ("MLC000", include_str!("../docs/rules/MLC000.md")),
        ("MLC001", include_str!("../docs/rules/MLC001.md")),
    ];
    let normalized = rule.to_ascii_uppercase();
    if let Some((_, text)) = DOCS.iter().find(|(id, _)| *id == normalized) {
        return Ok(Execution::success((*text).to_owned()));
    }
    if registry::is_reserved_rule_id(&normalized) {
        let phase = registry::implementation_phase(&normalized)
            .map_or_else(String::new, |phase| format!(" (planned for phase {phase})"));
        return Ok(Execution::success(format!(
            "{normalized} is reserved and not implemented yet{phase}. \
             It is never reported as checked.\n"
        )));
    }
    Err(ApplicationError::Cli(format!("unknown rule: {rule}")))
}

fn render_rules() -> String {
    let mut output = String::new();
    for rule_id in registry::all_reserved_rule_ids() {
        let phase = registry::implementation_phase(&rule_id)
            .map_or_else(|| "?".to_owned(), |phase| phase.to_string());
        let status = if registry::is_implemented(&rule_id) {
            "implemented"
        } else {
            "reserved"
        };
        let summary = registry::metadata_for(&rule_id).map_or("", |metadata| metadata.summary);
        if summary.is_empty() {
            let _ = writeln!(output, "{rule_id}  phase {phase}  {status}");
        } else {
            let _ = writeln!(output, "{rule_id}  phase {phase}  {status}  {summary}");
        }
    }
    output
}

fn run_config() -> Result<Execution, ApplicationError> {
    let current_dir = std::env::current_dir().map_err(|source| ApplicationError::Io {
        path: PathBuf::from("."),
        source,
    })?;
    let args = CheckArgs::default();
    let config = resolve_config(&args, &discover_project_root(&[current_dir])?)?;
    let mut output = serde_json::to_string_pretty(&config)
        .map_err(|error| ApplicationError::Cli(error.to_string()))?;
    output.push('\n');
    Ok(Execution::success(output))
}
