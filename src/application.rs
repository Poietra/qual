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
use crate::frontend::{self, ManimSurface};
use crate::knowledge::{self, KnowledgeProfile, SymbolKind};
use crate::reporting::fixes::FixReport;
use crate::reporting::{self, RenderContext, baseline, fixes, suppressions};
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
    /// Fix application failed while writing a fixed file.
    #[error(transparent)]
    Fix(#[from] fixes::FixError),
    /// The configured knowledge profile is unknown or invalid
    /// (DESIGN §8.2: configuration-level failure, exit code 2).
    #[error(transparent)]
    Knowledge(#[from] knowledge::KnowledgeError),
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
    /// Final (filtered, suppressed, sorted, baseline-filtered) diagnostics.
    pub diagnostics: Vec<Diagnostic>,
    /// Rendered output in the requested format.
    pub output: String,
    /// Exit status implied by `fail-level` and `min-confidence`.
    pub exit: ExitStatus,
    /// The resolved configuration used for the run.
    pub config: ResolvedConfig,
    /// Fix-application summary when `--fix` was given.
    pub fixes: Option<FixReport>,
}

/// Runs `manim-lint check` and renders its output.
pub fn run_check(args: &CheckArgs) -> Result<Execution, ApplicationError> {
    validate_flag_combinations(args)?;
    let report = check(args)?;
    let mut stderr = String::new();
    if args.statistics {
        stderr.push_str(&render_statistics(&report.diagnostics));
    }
    if let Some(fix_report) = &report.fixes {
        if !fix_report.is_empty() {
            stderr.push_str(&render_fix_summary(fix_report));
        }
    }
    Ok(Execution {
        stdout: report.output,
        stderr: (!stderr.is_empty()).then_some(stderr),
        exit: report.exit,
    })
}

/// Full `check` pipeline as a library entry point (used by tests).
pub fn check(args: &CheckArgs) -> Result<CheckReport, ApplicationError> {
    let paths = normalized_input_paths(args)?;
    let project_root = discover_project_root(&paths)?;
    let config = resolve_config(args, &project_root)?;

    // Load the versioned Manim knowledge profile before touching any file:
    // an unknown `knowledge-profile` is a configuration error (exit 2).
    let profile_name = config
        .knowledge_profile
        .clone()
        .unwrap_or_else(|| knowledge::DEFAULT_PROFILE.to_owned());
    let profile = knowledge::load(&profile_name)?;

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

    let surface = manim_surface(&profile);
    let facts = frontend::index::analyze(&sources, &config.source_roots, &surface);
    let context = RuleContext::new(&sources, &config)
        .with_knowledge(&profile)
        .with_frontend(facts.index, facts.calls);
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

    // `--write-baseline` records the diagnostics as computed above, before
    // any `--baseline` filtering, so old entries are never dropped when
    // both flags are combined.
    if let Some(path) = &args.write_baseline {
        let document = baseline::render(&diagnostics, &sources);
        std::fs::write(path, document).map_err(|source| ApplicationError::Io {
            path: path.clone(),
            source,
        })?;
    }

    // `--baseline` filters already-known diagnostics out before rendering
    // and before the exit code is computed.
    if let Some(path) = &args.baseline {
        let text = std::fs::read_to_string(path).map_err(|source| ApplicationError::Io {
            path: path.clone(),
            source,
        })?;
        let known = baseline::Baseline::parse(&text).map_err(|message| {
            ApplicationError::Cli(format!(
                "cannot use baseline {path}: {message}",
                path = path.display()
            ))
        })?;
        diagnostics = known.filter(diagnostics, &sources);
    }

    let fix_report = if args.fix {
        Some(fixes::apply(&sources, &diagnostics, args.unsafe_fixes)?)
    } else {
        None
    };

    let profiles = config.active_profile_names();
    let render_context = RenderContext {
        tool_version: crate::VERSION,
        project_root: ".",
        profiles: &profiles,
    };
    let output = reporting::render(args.format, &diagnostics, &render_context);

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
        fixes: fix_report,
    })
}

/// Derives the frontend's Manim API surface from the loaded knowledge
/// profile (DESIGN §5.3/§5.4 bridge).
///
/// `star_exports` is the profile's export table verbatim. The base sets are
/// derived from curated symbol kinds — every canonical class of a kind counts
/// as a base of that kind, so a project class is discovered as e.g. a Scene
/// only when its resolved base chain reaches one of these ids. `vmobject`
/// symbols are also Mobject bases (a `VMobject` subclass is a Mobject).
#[must_use]
pub fn manim_surface(profile: &KnowledgeProfile) -> ManimSurface {
    let mut surface = ManimSurface {
        star_exports: profile.exports.clone(),
        ..ManimSurface::default()
    };
    for (id, entry) in &profile.symbols {
        match entry.kind {
            SymbolKind::Scene => {
                surface.scene_bases.insert(id.clone());
            }
            SymbolKind::Mobject | SymbolKind::Vmobject => {
                surface.mobject_bases.insert(id.clone());
            }
            SymbolKind::Animation => {
                surface.animation_bases.insert(id.clone());
            }
            SymbolKind::Camera
            | SymbolKind::Function
            | SymbolKind::Method
            | SymbolKind::Constant => {}
        }
    }
    surface
}

fn validate_flag_combinations(args: &CheckArgs) -> Result<(), ApplicationError> {
    if args.unsafe_fixes && !args.fix {
        return Err(ApplicationError::Cli(
            "--unsafe-fixes requires --fix".to_owned(),
        ));
    }
    // --no-cache is an accepted no-op: no cache exists yet.
    Ok(())
}

fn render_fix_summary(report: &FixReport) -> String {
    let mut output = String::new();
    let _ = writeln!(
        output,
        "fixed {applied} issue(s) in {files} file(s)",
        applied = report.applied,
        files = report.files_changed.len()
    );
    if report.skipped_unsafe > 0 {
        let _ = writeln!(
            output,
            "skipped {count} unsafe fix(es); re-run with --unsafe-fixes to apply them",
            count = report.skipped_unsafe
        );
    }
    if report.skipped_overlapping > 0 {
        let _ = writeln!(
            output,
            "skipped {count} fix(es) with overlapping edits",
            count = report.skipped_overlapping
        );
    }
    if report.skipped_invalid > 0 {
        let _ = writeln!(
            output,
            "skipped {count} fix(es) with unresolvable edit spans",
            count = report.skipped_invalid
        );
    }
    for rolled_back in &report.rolled_back {
        let _ = writeln!(
            output,
            "rolled back {path}: {reason}",
            path = rolled_back.path,
            reason = rolled_back.reason
        );
    }
    output
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
