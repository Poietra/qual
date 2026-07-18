//! Command orchestration (DESIGN §8.1): resolve config, collect files,
//! parse, run rules, apply suppressions, filter, sort, render.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::cli::{CheckArgs, Command, ExitStatus};
use crate::config::loader::{self, ProfileSelection, ResolutionInput};
use crate::config::model::{ConfigFragment, ResolvedConfig};
use crate::cost;
use crate::diagnostic::Diagnostic;
use crate::frontend::{self, ManimSurface};
use crate::knowledge::{self, KnowledgeProfile, SymbolKind};
use crate::reporting::fixes::FixReport;
use crate::reporting::{self, RenderContext, baseline, fixes, suppressions};
use crate::rules::RuleContext;
use crate::rules::registry;
use crate::semantic;
use crate::semantic::interpreter::SceneLifecycle;
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
        Command::Cost { path, scene } => run_cost(&path, scene.as_deref()),
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

    let facts = compute_facts(&sources, &config, &profile);
    let context = RuleContext::new(&sources, &config)
        .with_knowledge(&profile)
        .with_frontend(facts.index, facts.calls)
        .with_lifecycle(facts.lifecycle)
        .with_cost(facts.cost);
    for rule in registry::all_rules() {
        diagnostics.extend(rule.run(&context));
    }

    // Supersession runs BEFORE suppression filtering, deliberately: the
    // specificity dedup (DESIGN §7.3) is part of diagnostic *production* —
    // the specific diagnostic permanently replaces the generic one as the
    // single report of the finding. Inline-suppressing the specific rule
    // therefore silences the finding entirely instead of resurrecting a
    // generic duplicate the user never saw (which would demand stacked
    // ignores for one defect). This also keeps the shared pass consistent
    // with rule-internal pre-filters (e.g. MLP201 excluding MLP226's
    // frame-varying calls), which never resurrect either.
    diagnostics = apply_supersedes(diagnostics);

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

/// The full fact stack a `check` run feeds to the rules.
struct ProjectFacts {
    index: frontend::index::ProjectIndex,
    calls: frontend::index::QualifiedCallFacts,
    lifecycle: semantic::interpreter::LifecycleFacts,
    cost: cost::CostFacts,
}

/// Runs the fixed fact-layer pipeline (DESIGN §5.1): frontend facts, then
/// the lifecycle abstract interpreter over the discovered scenes, then the
/// symbolic cost model. Rules only ever see the result.
fn compute_facts(
    sources: &SourceManager,
    config: &ResolvedConfig,
    profile: &KnowledgeProfile,
) -> ProjectFacts {
    let surface = manim_surface(profile);
    let facts = frontend::index::analyze(sources, &config.source_roots, &surface);
    let lifecycle =
        semantic::interpreter::analyze(sources, &facts.index, &facts.calls, Some(profile));
    let cost = cost::CostFacts::compute_with_lifecycle(
        sources,
        &facts.index,
        &facts.calls,
        Some(profile),
        &config.active_profiles,
        &lifecycle,
    );
    ProjectFacts {
        index: facts.index,
        calls: facts.calls,
        lifecycle,
        cost,
    }
}

/// Runs `manim-lint cost PATH [--scene NAME]` (DESIGN §8.1, §4.1): a
/// per-scene symbolic cost breakdown over the lifecycle and cost facts.
///
/// Unknown quantities are printed as "unknown" / "per-frame", never as a
/// number (DESIGN §15 invariant 9); an unknown `--scene` name is a CLI
/// error (exit code 2).
pub fn run_cost(path: &Path, scene_filter: Option<&str>) -> Result<Execution, ApplicationError> {
    let args = CheckArgs {
        paths: vec![path.to_path_buf()],
        ..CheckArgs::default()
    };
    let paths = normalized_input_paths(&args)?;
    let project_root = discover_project_root(&paths)?;
    let config = resolve_config(&args, &project_root)?;
    let profile_name = config
        .knowledge_profile
        .clone()
        .unwrap_or_else(|| knowledge::DEFAULT_PROFILE.to_owned());
    let profile = knowledge::load(&profile_name)?;
    let files = collect_python_files(&paths, &project_root, &config.exclude)?;
    let mut sources = SourceManager::new(project_root);
    for file in &files {
        sources.load_file(file);
    }
    let facts = compute_facts(&sources, &config, &profile);
    let output = render_cost_report(&sources, &config, &facts, scene_filter)?;
    Ok(Execution::success(output))
}

/// Renders the per-scene cost report over the computed fact stack.
fn render_cost_report(
    sources: &SourceManager,
    config: &ResolvedConfig,
    facts: &ProjectFacts,
    scene_filter: Option<&str>,
) -> Result<String, ApplicationError> {
    let scenes: Vec<&SceneLifecycle> = facts
        .lifecycle
        .scenes
        .iter()
        .filter(|scene| {
            scene_filter.is_none_or(|name| {
                scene.qualified_name == name
                    || scene.qualified_name.rsplit('.').next() == Some(name)
            })
        })
        .collect();
    if let Some(name) = scene_filter {
        if scenes.is_empty() {
            let known: Vec<&str> = facts
                .lifecycle
                .scenes
                .iter()
                .map(|scene| scene.qualified_name.as_str())
                .collect();
            return Err(ApplicationError::Cli(if known.is_empty() {
                format!("unknown scene: {name} (no scenes were discovered)")
            } else {
                format!(
                    "unknown scene: {name} (discovered scenes: {})",
                    known.join(", ")
                )
            }));
        }
    }

    let mut output = String::new();
    let profile_list = config
        .active_profiles
        .iter()
        .map(|profile| {
            format!(
                "{name} ({renderer}, {width}x{height}, {fps} fps)",
                name = profile.name,
                renderer = profile.renderer,
                width = profile.pixel_width,
                height = profile.pixel_height,
                fps = profile.frame_rate,
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(output, "profiles: {profile_list}");
    if scenes.is_empty() {
        let _ = writeln!(output, "no scenes discovered");
        return Ok(output);
    }
    for scene in scenes {
        render_scene_cost(&mut output, sources, config, facts, scene);
    }
    Ok(output)
}

/// Renders one scene section of the cost report.
#[allow(
    clippy::too_many_lines,
    reason = "one linear block per report section; splitting adds no clarity"
)]
fn render_scene_cost(
    output: &mut String,
    sources: &SourceManager,
    config: &ResolvedConfig,
    facts: &ProjectFacts,
    scene: &SceneLifecycle,
) {
    use crate::rules::performance::support::{display_frames, display_seconds, scene_frames_after};
    use crate::semantic::interpreter::PlayKind;

    let profiles = &config.active_profiles;
    let scene_file = sources.file(scene.file);
    let _ = writeln!(output);
    let _ = writeln!(
        output,
        "scene {name} ({path})",
        name = scene.qualified_name,
        path = scene_file.relative_path(),
    );

    // Play list: one row per direct lifecycle play, with the frame estimate
    // joined across the analyzed profiles.
    let _ = writeln!(output, "  plays:");
    if scene.plays.is_empty() {
        let _ = writeln!(output, "    (none)");
    }
    for play in &scene.plays {
        let kind = match play.kind {
            PlayKind::Play => "play",
            PlayKind::Wait => "wait",
        };
        let frames = cost::estimator::frames_across_profiles(&play.duration, profiles);
        let _ = writeln!(
            output,
            "    {location} {kind} duration {duration} -> frames {frames}",
            location = cost_location(sources, play.site.file, play.site.start),
            duration = display_seconds(&play.duration),
            frames = display_frames(&frames),
        );
    }

    // Hot contexts entered from this scene class: entry kind, provenance
    // chain, and non-neutral multiplicity factors.
    let _ = writeln!(output, "  hot contexts:");
    let mut context_lines: Vec<String> = Vec::new();
    for (call_index, contexts) in &facts.cost.hot.call_contexts {
        let call = &facts.calls.calls[*call_index];
        if call.context.class_name.as_deref() != Some(scene.qualified_name.as_str()) {
            continue;
        }
        for context in contexts {
            let Some(entry_step) = context.chain.first() else {
                continue;
            };
            let factors = cost::estimator::multiplicity_factor_names(&context.multiplicity);
            let line = format!(
                "    {location} entry {entry}; path {path}; factors {factors}",
                location = cost_location(
                    sources,
                    entry_step.file,
                    u32::from(entry_step.range.start())
                ),
                entry = context.entry.label(),
                path = context.state_path().join(" -> "),
                factors = if factors.is_empty() {
                    "none".to_owned()
                } else {
                    factors.join(" x ")
                },
            );
            if !context_lines.contains(&line) {
                context_lines.push(line);
            }
        }
    }
    if context_lines.is_empty() {
        let _ = writeln!(output, "    (none)");
    }
    for line in &context_lines {
        let _ = writeln!(output, "{line}");
    }

    // Per-frame constructions reachable from those contexts.
    let _ = writeln!(output, "  per-frame constructions:");
    let mut construction_rows = 0_usize;
    for construction in facts.cost.constructions_in_hot_contexts() {
        let call = &facts.calls.calls[construction.call_index];
        if call.context.class_name.as_deref() != Some(scene.qualified_name.as_str()) {
            continue;
        }
        construction_rows += 1;
        let invocations = scene_frames_after(scene, call, profiles).map_or_else(
            || "per-frame".to_owned(),
            |frames| {
                format!(
                    "{} invocations across literal plays",
                    display_frames(&frames)
                )
            },
        );
        let _ = writeln!(
            output,
            "    {location} {class} construction x {invocations}",
            location = cost_location(sources, call.file, u32::from(call.call_range.start())),
            class = short_class_name(&construction.symbol),
        );
    }
    if construction_rows == 0 {
        let _ = writeln!(output, "    (none)");
    }

    // Frame-varying resource keys: distinct Text/TeX/SVG cache keys grow
    // with the frame count (`K_resource ≈ F`).
    let _ = writeln!(output, "  resource-key growth:");
    let mut resource_rows = 0_usize;
    for fact in facts.cost.frame_varying_resource_keys() {
        let call = &facts.calls.calls[fact.call_index];
        if call.context.class_name.as_deref() != Some(scene.qualified_name.as_str()) {
            continue;
        }
        resource_rows += 1;
        let keys = scene_frames_after(scene, call, profiles).map_or_else(
            || "one per rendered frame".to_owned(),
            |frames| format!("{} across literal plays", display_frames(&frames)),
        );
        let _ = writeln!(
            output,
            "    {location} {class} distinct cache keys: {keys} (f-string key varies per frame)",
            location = cost_location(sources, call.file, u32::from(call.call_range.start())),
            class = short_class_name(&fact.symbol),
        );
    }
    if resource_rows == 0 {
        let _ = writeln!(output, "    (none)");
    }
}

/// `path:line:column` of a byte offset, for cost-report rows.
fn cost_location(sources: &SourceManager, file: crate::source::FileId, byte: u32) -> String {
    let source_file = sources.file(file);
    let position = source_file.position_of_byte(byte as usize);
    format!(
        "{path}:{line}:{column}",
        path = source_file.relative_path(),
        line = position.line,
        column = position.column,
    )
}

/// The unqualified class name of a canonical id.
fn short_class_name(canonical: &str) -> &str {
    canonical.rsplit('.').next().unwrap_or(canonical)
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

/// Sortable bucket key of a diagnostic's primary location: supersession
/// only ever collapses diagnostics with the same path AND the same span.
fn supersedes_bucket_key(diagnostic: &Diagnostic) -> (&str, usize, usize, usize, usize) {
    (
        diagnostic.path.as_str(),
        diagnostic.primary_span.start.line,
        diagnostic.primary_span.start.column,
        diagnostic.primary_span.end.line,
        diagnostic.primary_span.end.column,
    )
}

/// Specificity dedup (DESIGN §7.3 end): when two diagnostics share a
/// primary span and the rule of one declares `supersedes` over the rule of
/// the other, only the more specific one is reported. Individual rules may
/// additionally pre-filter (MLP201 excludes MLP226's frame-varying calls
/// where the facts are computed); this shared pass makes the guarantee
/// uniform for every declared pair regardless of where each rule anchors.
///
/// Diagnostics are bucketed by `(path, primary span)` first, so each
/// diagnostic only consults the rules reported at its own location
/// instead of scanning all n diagnostics (the pass is O(n log n), not
/// O(n²)).
fn apply_supersedes(diagnostics: Vec<Diagnostic>) -> Vec<Diagnostic> {
    let mut superseded_ids_by_bucket: BTreeMap<
        (&str, usize, usize, usize, usize),
        Vec<&'static str>,
    > = BTreeMap::new();
    for diagnostic in &diagnostics {
        let Some(metadata) = registry::metadata_for(&diagnostic.rule_id) else {
            continue;
        };
        if metadata.supersedes.is_empty() {
            continue;
        }
        superseded_ids_by_bucket
            .entry(supersedes_bucket_key(diagnostic))
            .or_default()
            .extend(metadata.supersedes);
    }
    let keep: Vec<bool> = diagnostics
        .iter()
        .map(|diagnostic| {
            superseded_ids_by_bucket
                .get(&supersedes_bucket_key(diagnostic))
                .is_none_or(|ids| !ids.contains(&diagnostic.rule_id.as_str()))
        })
        .collect();
    diagnostics
        .into_iter()
        .zip(keep)
        .filter_map(|(diagnostic, kept)| kept.then_some(diagnostic))
        .collect()
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
    const DOCS: [(&str, &str); 79] = [
        ("MLC000", include_str!("../docs/rules/MLC000.md")),
        ("MLC001", include_str!("../docs/rules/MLC001.md")),
        ("MLC101", include_str!("../docs/rules/MLC101.md")),
        ("MLC102", include_str!("../docs/rules/MLC102.md")),
        ("MLC103", include_str!("../docs/rules/MLC103.md")),
        ("MLC104", include_str!("../docs/rules/MLC104.md")),
        ("MLC105", include_str!("../docs/rules/MLC105.md")),
        ("MLC106", include_str!("../docs/rules/MLC106.md")),
        ("MLC107", include_str!("../docs/rules/MLC107.md")),
        ("MLC108", include_str!("../docs/rules/MLC108.md")),
        ("MLC109", include_str!("../docs/rules/MLC109.md")),
        ("MLC110", include_str!("../docs/rules/MLC110.md")),
        ("MLC111", include_str!("../docs/rules/MLC111.md")),
        ("MLC112", include_str!("../docs/rules/MLC112.md")),
        ("MLC113", include_str!("../docs/rules/MLC113.md")),
        ("MLC115", include_str!("../docs/rules/MLC115.md")),
        ("MLC117", include_str!("../docs/rules/MLC117.md")),
        ("MLC119", include_str!("../docs/rules/MLC119.md")),
        ("MLC120", include_str!("../docs/rules/MLC120.md")),
        ("MLC121", include_str!("../docs/rules/MLC121.md")),
        ("MLC122", include_str!("../docs/rules/MLC122.md")),
        ("MLC123", include_str!("../docs/rules/MLC123.md")),
        ("MLC124", include_str!("../docs/rules/MLC124.md")),
        ("MLC125", include_str!("../docs/rules/MLC125.md")),
        ("MLC126", include_str!("../docs/rules/MLC126.md")),
        ("MLC127", include_str!("../docs/rules/MLC127.md")),
        ("MLC128", include_str!("../docs/rules/MLC128.md")),
        ("MLC129", include_str!("../docs/rules/MLC129.md")),
        ("MLR101", include_str!("../docs/rules/MLR101.md")),
        ("MLR102", include_str!("../docs/rules/MLR102.md")),
        ("MLR103", include_str!("../docs/rules/MLR103.md")),
        ("MLR104", include_str!("../docs/rules/MLR104.md")),
        ("MLR105", include_str!("../docs/rules/MLR105.md")),
        ("MLR106", include_str!("../docs/rules/MLR106.md")),
        ("MLR107", include_str!("../docs/rules/MLR107.md")),
        ("MLR108", include_str!("../docs/rules/MLR108.md")),
        ("MLR110", include_str!("../docs/rules/MLR110.md")),
        ("MLR111", include_str!("../docs/rules/MLR111.md")),
        ("MLR112", include_str!("../docs/rules/MLR112.md")),
        ("MLR113", include_str!("../docs/rules/MLR113.md")),
        ("MLR114", include_str!("../docs/rules/MLR114.md")),
        ("MLR115", include_str!("../docs/rules/MLR115.md")),
        ("MLR116", include_str!("../docs/rules/MLR116.md")),
        ("MLR117", include_str!("../docs/rules/MLR117.md")),
        ("MLR119", include_str!("../docs/rules/MLR119.md")),
        ("MLR120", include_str!("../docs/rules/MLR120.md")),
        ("MLR121", include_str!("../docs/rules/MLR121.md")),
        ("MLR124", include_str!("../docs/rules/MLR124.md")),
        ("MLR125", include_str!("../docs/rules/MLR125.md")),
        ("MLR126", include_str!("../docs/rules/MLR126.md")),
        ("MLR127", include_str!("../docs/rules/MLR127.md")),
        ("MLD301", include_str!("../docs/rules/MLD301.md")),
        ("MLD302", include_str!("../docs/rules/MLD302.md")),
        ("MLD303", include_str!("../docs/rules/MLD303.md")),
        ("MLD304", include_str!("../docs/rules/MLD304.md")),
        ("MLD305", include_str!("../docs/rules/MLD305.md")),
        ("MLD306", include_str!("../docs/rules/MLD306.md")),
        ("MLD307", include_str!("../docs/rules/MLD307.md")),
        ("MLP201", include_str!("../docs/rules/MLP201.md")),
        ("MLP202", include_str!("../docs/rules/MLP202.md")),
        ("MLP203", include_str!("../docs/rules/MLP203.md")),
        ("MLP204", include_str!("../docs/rules/MLP204.md")),
        ("MLP205", include_str!("../docs/rules/MLP205.md")),
        ("MLP206", include_str!("../docs/rules/MLP206.md")),
        ("MLP207", include_str!("../docs/rules/MLP207.md")),
        ("MLP208", include_str!("../docs/rules/MLP208.md")),
        ("MLP209", include_str!("../docs/rules/MLP209.md")),
        ("MLP210", include_str!("../docs/rules/MLP210.md")),
        ("MLP211", include_str!("../docs/rules/MLP211.md")),
        ("MLP215", include_str!("../docs/rules/MLP215.md")),
        ("MLP216", include_str!("../docs/rules/MLP216.md")),
        ("MLP218", include_str!("../docs/rules/MLP218.md")),
        ("MLP219", include_str!("../docs/rules/MLP219.md")),
        ("MLP220", include_str!("../docs/rules/MLP220.md")),
        ("MLP221", include_str!("../docs/rules/MLP221.md")),
        ("MLP222", include_str!("../docs/rules/MLP222.md")),
        ("MLP224", include_str!("../docs/rules/MLP224.md")),
        ("MLP226", include_str!("../docs/rules/MLP226.md")),
        ("MLP227", include_str!("../docs/rules/MLP227.md")),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::{Confidence, Severity, SourcePosition, SourceSpan};

    fn span(line: usize, column: usize) -> SourceSpan {
        SourceSpan {
            start: SourcePosition { line, column },
            end: SourcePosition {
                line,
                column: column + 4,
            },
        }
    }

    fn diagnostic(rule_id: &str, path: &str, at: SourceSpan) -> Diagnostic {
        Diagnostic {
            rule_id: rule_id.to_owned(),
            severity: Severity::Warning,
            confidence: Confidence::High,
            path: path.to_owned(),
            primary_span: at,
            message: String::new(),
            explanation: None,
            related_locations: Vec::new(),
            evidence: BTreeMap::new(),
            estimated_cost: None,
            applicable_profiles: Vec::new(),
            fix: None,
        }
    }

    fn ids(diagnostics: &[Diagnostic]) -> Vec<&str> {
        diagnostics
            .iter()
            .map(|diagnostic| diagnostic.rule_id.as_str())
            .collect()
    }

    /// Same path, same primary span: the declared specific rule (MLP226
    /// supersedes MLP201) is the only survivor, in either input order.
    #[test]
    fn apply_supersedes_collapses_declared_pairs_on_one_span() {
        let here = span(7, 39);
        let collapsed = apply_supersedes(vec![
            diagnostic("MLP201", "scene.py", here),
            diagnostic("MLP226", "scene.py", here),
        ]);
        assert_eq!(ids(&collapsed), ["MLP226"]);
        let collapsed = apply_supersedes(vec![
            diagnostic("MLP226", "scene.py", here),
            diagnostic("MLP201", "scene.py", here),
        ]);
        assert_eq!(ids(&collapsed), ["MLP226"]);
    }

    /// The wave-6 cardinality pairs collapse the same way: `MLP224`
    /// (`point_from_proportion`) supersedes the generic family walk
    /// `MLP203`, and `MLP208` (Text/TeX transform) supersedes the
    /// generic `MLP207`, on a shared span in either input order.
    #[test]
    fn apply_supersedes_collapses_the_cardinality_pairs_on_one_span() {
        let here = span(12, 21);
        let collapsed = apply_supersedes(vec![
            diagnostic("MLP203", "scene.py", here),
            diagnostic("MLP224", "scene.py", here),
        ]);
        assert_eq!(ids(&collapsed), ["MLP224"]);
        let collapsed = apply_supersedes(vec![
            diagnostic("MLP208", "scene.py", here),
            diagnostic("MLP207", "scene.py", here),
        ]);
        assert_eq!(ids(&collapsed), ["MLP208"]);
    }

    /// A different span or a different file is never collapsed: the
    /// supersedes relation only holds for one primary location (DESIGN
    /// §7.3 "same primary span, same evidence").
    #[test]
    fn apply_supersedes_keeps_distinct_locations() {
        let kept = apply_supersedes(vec![
            diagnostic("MLP226", "scene.py", span(7, 39)),
            diagnostic("MLP201", "scene.py", span(9, 39)),
            diagnostic("MLP201", "other.py", span(7, 39)),
            // MLP220 supersedes MLP204, but only ever on a shared span;
            // anchored apart they are two distinct defects (see
            // rules::performance::traced_path).
            diagnostic("MLP220", "scene.py", span(11, 17)),
            diagnostic("MLP204", "scene.py", span(12, 37)),
        ]);
        assert_eq!(
            ids(&kept),
            ["MLP226", "MLP201", "MLP201", "MLP220", "MLP204"]
        );
    }

    /// Rules with no metadata (never produced by the registry) and rules
    /// with an empty supersedes list pass through untouched.
    #[test]
    fn apply_supersedes_ignores_undeclared_rules() {
        let here = span(3, 1);
        let kept = apply_supersedes(vec![
            diagnostic("MLC101", "scene.py", here),
            diagnostic("MLR104", "scene.py", here),
        ]);
        assert_eq!(ids(&kept), ["MLC101", "MLR104"]);
    }
}
