//! Command orchestration (DESIGN §8.1): resolve config, collect files,
//! parse, run rules, apply suppressions, filter, sort, render.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use rayon::prelude::*;
use sha2::Digest;

use crate::cache::{
    AnalysisCache, AnalysisComponent, CacheKey, CacheStatus, ComponentCacheEntry,
    DependencyManifest,
};
use crate::change_impact::{self as change_impact_projection, SnapshotInput};
use crate::cli::{
    ChangeImpactArgs, CheckArgs, ColorMode, Command, ExitStatus, SourceBridgeArgs, StaticFactsArgs,
};
use crate::config::loader::{self, ProfileSelection, ResolutionInput};
use crate::config::model::{ConfigFragment, ResolvedConfig};
use crate::cost;
use crate::diagnostic::Diagnostic;
use crate::frontend::index::LiteralFact;
use crate::frontend::{self, ManimSurface};
use crate::knowledge::{self, KnowledgeProfile, SymbolKind};
use crate::reporting::coverage::{self, CoverageFormat, CoverageReport};
use crate::reporting::fixes::FixReport;
use crate::reporting::rich::ColorChoice;
use crate::reporting::{self, OutputFormat, RenderContext, baseline, fixes, suppressions};
use crate::rules::RuleContext;
use crate::rules::registry;
use crate::semantic;
use crate::semantic::interpreter::SceneLifecycle;
use crate::semantic::summaries::SummaryTable;
use crate::source::{FileId, SourceManager};
use crate::source_bridge::{self, GenerationInput, PatchCandidate, PatchEdit, PatchRequest};
use crate::static_facts::{self as static_facts_projection, ProjectionInput};

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
        Command::Coverage { paths, format } => run_coverage(&paths, format),
        Command::StaticFacts(args) => run_static_facts(&args),
        Command::ChangeImpact(args) => run_change_impact(&args),
        Command::SourceBridge(args) => run_source_bridge(&args),
    }
}

/// Outcome of one non-writing source bridge run.
#[derive(Debug)]
pub struct SourceBridgeReport {
    /// Parsed public document.
    pub document: serde_json::Value,
    /// Canonical deterministic JSON including one trailing newline.
    pub output: String,
}

/// Runs `manim-lint source-bridge PATH --request REQUEST.json`.
pub fn run_source_bridge(args: &SourceBridgeArgs) -> Result<Execution, ApplicationError> {
    let report = source_bridge(args)?;
    Ok(Execution::success(report.output))
}

/// Generates and virtually validates bounded source patches without writing
/// project files.
pub fn source_bridge(args: &SourceBridgeArgs) -> Result<SourceBridgeReport, ApplicationError> {
    let request_bytes = std::fs::read(&args.request).map_err(|source| ApplicationError::Io {
        path: args.request.clone(),
        source,
    })?;
    let request: PatchRequest = serde_json::from_slice(&request_bytes).map_err(|error| {
        ApplicationError::Cli(format!("invalid source bridge request JSON: {error}"))
    })?;
    request.validate_contract().map_err(|error| {
        ApplicationError::Cli(format!("invalid source bridge request: {error}"))
    })?;
    let snapshot = analyze_semantic_snapshot(
        &args.path,
        args.profile.as_deref(),
        args.renderer,
        args.fps,
        args.resolution,
    )?;
    let generated = source_bridge::generate(
        &GenerationInput {
            sources: &snapshot.sources,
            raw_sources: &snapshot.raw_sources,
            calls: &snapshot.calls,
            lifecycle: &snapshot.lifecycle,
            static_facts: &snapshot.static_facts,
        },
        &request,
    );
    let profile_name = snapshot
        .config
        .knowledge_profile
        .clone()
        .unwrap_or_else(|| knowledge::DEFAULT_PROFILE.to_owned());
    let profile = knowledge::load(&profile_name)?;
    let mut candidates = Vec::new();
    let mut accepted = 0usize;
    for candidate in &generated.candidates {
        let validation = validate_patch_candidate(&snapshot, candidate, &request, &profile);
        if validation["status"] == "accepted" {
            accepted += 1;
        }
        candidates.push(source_bridge::candidate_value(
            candidate,
            &validation,
            &snapshot.sources,
            &snapshot.raw_sources,
        ));
    }
    let status = match accepted {
        0 => "unavailable",
        1 => "unique",
        _ => "ambiguous",
    };
    let document = serde_json::json!({
        "schema_version": 0,
        "tool": {
            "name": "manim-lint",
            "version": crate::VERSION,
            "semantic_build_hash": snapshot.static_facts.document["tool"]["semantic_build_hash"],
        },
        "snapshot": {
            "id": snapshot.static_facts.document["snapshot"]["id"],
            "source_manifest_hash": snapshot.static_facts.document["snapshot"]["source_manifest_hash"],
            "semantic_config_hash": snapshot.static_facts.document["snapshot"]["semantic_config_hash"],
        },
        "request": {
            "target_id": request.target_id,
            "operation": source_bridge::operation_name(&request.operation),
        },
        "status": status,
        "candidates": candidates,
        "unknowns": generated.unknowns,
    });
    let mut output = serde_json::to_string_pretty(&document)
        .expect("source bridge projection contains finite JSON");
    output.push('\n');
    Ok(SourceBridgeReport { document, output })
}

/// Outcome of a `change-impact` run for library callers and schema tests.
#[derive(Debug)]
pub struct ChangeImpactReport {
    /// Parsed public document.
    pub document: serde_json::Value,
    /// Canonical deterministic JSON including one trailing newline.
    pub output: String,
    /// Resolved configuration for the base snapshot.
    pub base_config: ResolvedConfig,
    /// Resolved configuration for the target snapshot.
    pub target_config: ResolvedConfig,
}

/// Runs `manim-lint change-impact --before OLD --after NEW`.
pub fn run_change_impact(args: &ChangeImpactArgs) -> Result<Execution, ApplicationError> {
    let report = change_impact(args)?;
    Ok(Execution::success(report.output))
}

/// Computes `ChangeImpact` v0 without running diagnostic rules or using the
/// analysis cache.
pub fn change_impact(args: &ChangeImpactArgs) -> Result<ChangeImpactReport, ApplicationError> {
    let base = analyze_semantic_snapshot(
        &args.before,
        args.profile.as_deref(),
        args.renderer,
        args.fps,
        args.resolution,
    )?;
    let target = analyze_semantic_snapshot(
        &args.after,
        args.profile.as_deref(),
        args.renderer,
        args.fps,
        args.resolution,
    )?;
    let projected = change_impact_projection::compare(base.input(), target.input());
    Ok(ChangeImpactReport {
        document: projected.document,
        output: projected.json,
        base_config: base.config,
        target_config: target.config,
    })
}

struct AnalyzedSemanticSnapshot {
    sources: SourceManager,
    raw_sources: Vec<Vec<u8>>,
    calls: frontend::index::QualifiedCallFacts,
    lifecycle: semantic::interpreter::LifecycleFacts,
    graph: semantic::dependency::SemanticDependencyGraph,
    static_facts: static_facts_projection::StaticFactsOutput,
    config: ResolvedConfig,
}

impl AnalyzedSemanticSnapshot {
    fn input(&self) -> SnapshotInput<'_> {
        SnapshotInput {
            sources: &self.sources,
            raw_sources: &self.raw_sources,
            graph: &self.graph,
            static_facts: &self.static_facts,
        }
    }
}

fn analyze_semantic_snapshot(
    path: &Path,
    profile: Option<&str>,
    renderer: Option<crate::config::model::Renderer>,
    fps: Option<f64>,
    resolution: Option<crate::cli::Resolution>,
) -> Result<AnalyzedSemanticSnapshot, ApplicationError> {
    let check_args = CheckArgs {
        paths: vec![path.to_path_buf()],
        profile: profile.map(str::to_owned),
        renderer,
        fps,
        resolution,
        ..CheckArgs::default()
    };
    let paths = normalized_input_paths(&check_args)?;
    let project_root = discover_project_root(&paths)?;
    let config = resolve_config(&check_args, &project_root)?;
    let profile_name = config
        .knowledge_profile
        .clone()
        .unwrap_or_else(|| knowledge::DEFAULT_PROFILE.to_owned());
    let profile = knowledge::load(&profile_name)?;
    validate_declared_manim_version(&config, &profile)?;
    let files = collect_python_files(&paths, &project_root, &config.exclude)?;
    let mut raw_sources = Vec::with_capacity(files.len());
    for source_path in &files {
        raw_sources.push(
            std::fs::read(source_path).map_err(|source| ApplicationError::Io {
                path: source_path.clone(),
                source,
            })?,
        );
    }
    let mut sources = SourceManager::new(project_root);
    for (source_path, raw) in files.iter().zip(&raw_sources) {
        sources.load_bytes(source_path, raw);
    }
    Ok(analyze_loaded_semantic_snapshot(
        sources,
        raw_sources,
        config,
        &profile,
    ))
}

fn analyze_loaded_semantic_snapshot(
    sources: SourceManager,
    raw_sources: Vec<Vec<u8>>,
    config: ResolvedConfig,
    profile: &KnowledgeProfile,
) -> AnalyzedSemanticSnapshot {
    let facts = compute_facts(
        &sources,
        &config,
        profile,
        FactNeeds {
            lifecycle: true,
            cost: false,
        },
    );
    let mut graph = semantic::dependency::SemanticDependencyGraph::from_frontend(
        &sources,
        &config.source_roots,
        &facts.index,
        &facts.calls,
    );
    graph.attach_lifecycle(&facts.lifecycle, &sources, &facts.index);
    let static_facts = static_facts_projection::project(ProjectionInput {
        sources: &sources,
        raw_sources: &raw_sources,
        config: &config,
        knowledge: profile,
        index: &facts.index,
        calls: &facts.calls,
        lifecycle: &facts.lifecycle,
    });
    AnalyzedSemanticSnapshot {
        sources,
        raw_sources,
        calls: facts.calls,
        lifecycle: facts.lifecycle,
        graph,
        static_facts,
        config,
    }
}

fn validate_patch_candidate(
    before: &AnalyzedSemanticSnapshot,
    candidate: &PatchCandidate,
    request: &PatchRequest,
    profile: &KnowledgeProfile,
) -> serde_json::Value {
    let Some(edit) = candidate
        .edits
        .first()
        .filter(|_| candidate.edits.len() == 1)
    else {
        return rejected_validation(
            &request.target_id,
            "unsupported-edit-set",
            "v0 requires exactly one edit",
        );
    };
    let after = match analyze_virtual_edit(before, edit, profile) {
        Ok(after) => after,
        Err(detail) => {
            return rejected_validation(&request.target_id, "post-edit-parse-failed", &detail);
        }
    };
    let parsed = after
        .sources
        .files()
        .iter()
        .all(crate::source::SourceFile::is_parsed);
    let rematch = source_bridge::rematch(
        &before.static_facts.document,
        &after.static_facts.document,
        &request.target_id,
        edit,
    );
    let shift_frontier_allowance = if matches!(
        &request.operation,
        crate::source_bridge::PatchOperation::InsertShiftChain { .. }
    ) {
        source_bridge::inserted_shift_frontier_allowance(&after.static_facts.document, edit)
    } else {
        0
    };
    let allowed_frontiers = if shift_frontier_allowance > 0 {
        vec![(
            "call-resolution:dynamic-call-target",
            shift_frontier_allowance,
        )]
    } else {
        Vec::new()
    };
    let coverage = source_bridge::coverage_validation(
        &before.static_facts.document,
        &after.static_facts.document,
        &allowed_frontiers,
    );
    let mut reasons = Vec::new();
    if !parsed {
        reasons.push(serde_json::json!({ "kind": "post-edit-parse-failed" }));
    }
    match rematch["status"].as_str() {
        Some("match") => {}
        Some("ambiguous") => reasons.push(serde_json::json!({ "kind": "ambiguous-rematch" })),
        _ => reasons.push(serde_json::json!({ "kind": "missing-rematch" })),
    }
    if coverage["status"] == "decreased" {
        reasons.push(serde_json::json!({ "kind": "coverage-decreased" }));
    }
    serde_json::json!({
        "status": if reasons.is_empty() { "accepted" } else { "rejected" },
        "parse": if parsed { "valid" } else { "invalid" },
        "target_snapshot_id": after.static_facts.document["snapshot"]["id"],
        "rematch": rematch,
        "coverage": coverage,
        "reasons": reasons,
    })
}

fn rejected_validation(original_id: &str, kind: &str, detail: &str) -> serde_json::Value {
    serde_json::json!({
        "status": "rejected",
        "parse": "invalid",
        "rematch": {
            "status": "missing",
            "original_id": original_id,
            "candidate_ids": [],
        },
        "coverage": {
            "status": "decreased",
            "new_frontier_kinds": [],
        },
        "reasons": [{ "kind": kind, "detail": detail }],
    })
}

fn analyze_virtual_edit(
    before: &AnalyzedSemanticSnapshot,
    edit: &PatchEdit,
    profile: &KnowledgeProfile,
) -> Result<AnalyzedSemanticSnapshot, String> {
    let Some((index, source)) = before
        .sources
        .files()
        .iter()
        .enumerate()
        .find(|(_, source)| source.relative_path() == edit.path)
    else {
        return Err(format!("edit path is not in the snapshot: {}", edit.path));
    };
    let current_hash = format!(
        "sha256:{:x}",
        sha2::Sha256::digest(&before.raw_sources[index])
    );
    if current_hash != edit.raw_content_hash {
        return Err("raw source hash precondition failed".to_owned());
    }
    let Some(original) = source.text().get(edit.range.start..edit.range.end) else {
        return Err("edit range is not on UTF-8 character boundaries".to_owned());
    };
    if original != edit.original_text {
        return Err("rollback text precondition failed".to_owned());
    }
    let mut text = source.text().to_owned();
    text.replace_range(edit.range.start..edit.range.end, &edit.replacement);
    let encoded = fixes::encode(source, &text)?;
    let mut raw_sources = before.raw_sources.clone();
    raw_sources[index] = encoded;
    let mut sources = SourceManager::new(before.sources.project_root());
    for (source, raw) in before.sources.files().iter().zip(&raw_sources) {
        sources.load_bytes(source.path(), raw);
    }
    Ok(analyze_loaded_semantic_snapshot(
        sources,
        raw_sources,
        before.config.clone(),
        profile,
    ))
}

/// Outcome of a `static-facts` run for library callers and acceptance tests.
#[derive(Debug)]
pub struct StaticFactsReport {
    /// Parsed public document, suitable for schema validation or embedding.
    pub document: serde_json::Value,
    /// Canonical, deterministic pretty JSON (including one trailing newline).
    pub output: String,
    /// The resolved semantic configuration used for the snapshot.
    pub config: ResolvedConfig,
}

/// Runs `manim-lint static-facts [PATH...]` and writes `StaticFacts` v0 JSON.
pub fn run_static_facts(args: &StaticFactsArgs) -> Result<Execution, ApplicationError> {
    let report = static_facts(args)?;
    Ok(Execution::success(report.output))
}

/// Computes the public `StaticFacts` v0 projection without running lint rules.
///
/// Source files are read exactly once. The same immutable raw byte vectors
/// feed both Python decoding/parsing and public raw-content hashes, so an
/// on-disk race cannot create a self-inconsistent snapshot.
pub fn static_facts(args: &StaticFactsArgs) -> Result<StaticFactsReport, ApplicationError> {
    let check_args = CheckArgs {
        paths: args.paths.clone(),
        profile: args.profile.clone(),
        renderer: args.renderer,
        fps: args.fps,
        resolution: args.resolution,
        ..CheckArgs::default()
    };
    let paths = normalized_input_paths(&check_args)?;
    let project_root = discover_project_root(&paths)?;
    let config = resolve_config(&check_args, &project_root)?;
    let profile_name = config
        .knowledge_profile
        .clone()
        .unwrap_or_else(|| knowledge::DEFAULT_PROFILE.to_owned());
    let profile = knowledge::load(&profile_name)?;
    validate_declared_manim_version(&config, &profile)?;
    let files = collect_python_files(&paths, &project_root, &config.exclude)?;

    let mut raw_sources = Vec::with_capacity(files.len());
    for path in &files {
        raw_sources.push(std::fs::read(path).map_err(|source| ApplicationError::Io {
            path: path.clone(),
            source,
        })?);
    }
    let mut sources = SourceManager::new(project_root);
    for (path, raw) in files.iter().zip(&raw_sources) {
        sources.load_bytes(path, raw);
    }

    // The public contract is independent of configured rule selection. It
    // always asks for every fact layer in the StaticFacts contract and
    // projects those facts directly, without executing the diagnostic
    // registry. The symbolic cost model is not a v0 contract input.
    let facts = compute_facts(
        &sources,
        &config,
        &profile,
        FactNeeds {
            lifecycle: true,
            cost: false,
        },
    );
    let projected = static_facts_projection::project(ProjectionInput {
        sources: &sources,
        raw_sources: &raw_sources,
        config: &config,
        knowledge: &profile,
        index: &facts.index,
        calls: &facts.calls,
        lifecycle: &facts.lifecycle,
    });
    Ok(StaticFactsReport {
        document: projected.document,
        output: projected.json,
        config,
    })
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
    /// Analysis-coverage report when `--analysis-summary` was given.
    pub coverage: Option<CoverageReport>,
    /// Whether this run hit, partially reused, populated, or bypassed the
    /// analysis cache.
    pub cache_status: CacheStatus,
    /// Recoverable cache problems reported without aborting analysis.
    pub cache_warnings: Vec<String>,
}

struct ComponentPlan {
    component: AnalysisComponent,
    key: Option<CacheKey>,
    hit: bool,
    dependency_paths: BTreeSet<PathBuf>,
    dependencies_before: Option<DependencyManifest>,
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
    // The coverage summary is a stderr-only section after everything else:
    // it never changes stdout or the exit code (DESIGN §8.1 determinism).
    if let Some(coverage) = &report.coverage {
        stderr.push_str(&coverage::render_text(coverage));
    }
    for warning in &report.cache_warnings {
        let _ = writeln!(stderr, "manim-lint: warning: {warning}");
    }
    Ok(Execution {
        stdout: report.output,
        stderr: (!stderr.is_empty()).then_some(stderr),
        exit: report.exit,
    })
}

/// Full `check` pipeline as a library entry point (used by tests).
#[allow(
    clippy::too_many_lines,
    reason = "one linear pipeline stage per DESIGN §5.1 step; splitting adds no clarity"
)]
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
    validate_declared_manim_version(&config, &profile)?;

    let files = collect_python_files(&paths, &project_root, &config.exclude)?;
    let source_snapshot = read_source_snapshot(&files);
    let cache_eligible =
        cache_eligible(args) && source_snapshot.iter().all(|(_, bytes)| bytes.is_some());
    let mut analysis_cache = if cache_eligible {
        AnalysisCache::open(&project_root)
    } else {
        AnalysisCache::disabled()
    };
    let cache_key = if cache_eligible {
        let readable: Vec<(&Path, &[u8])> = source_snapshot
            .iter()
            .filter_map(|(path, bytes)| bytes.as_deref().map(|bytes| (path.as_path(), bytes)))
            .collect();
        match crate::cache::build_key(&project_root, &config, &profile, &readable) {
            Ok(key) => Some(key),
            Err(error) => {
                analysis_cache.disable_with_warning(format!(
                    "analysis cache key could not be built: {error}"
                ));
                None
            }
        }
    } else {
        None
    };
    if let Some(diagnostics) = cache_key
        .as_ref()
        .and_then(|key| analysis_cache.lookup(key))
    {
        return Ok(cached_check_report(
            args,
            config,
            diagnostics,
            &mut analysis_cache,
        ));
    }

    let mut sources = SourceManager::new(project_root.clone());
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut suppression_indexes = BTreeMap::new();

    for (path, bytes) in &source_snapshot {
        let id = match bytes {
            Some(bytes) => sources.load_bytes(path, bytes),
            None => sources.load_file(path),
        };
        let file = sources.file(id);
        if let Some(diagnostic) = file.parse_diagnostic() {
            let mut diagnostic = diagnostic.clone();
            frontend::features::append_pre37_async_hint(
                file,
                &config.target_python,
                &mut diagnostic,
            );
            diagnostics.push(diagnostic);
        }
        diagnostics.extend(frontend::features::gate(file, &config.target_python));
        let (index, warnings) = suppressions::collect(file);
        diagnostics.extend(warnings);
        if !index.is_empty() {
            suppression_indexes.insert(file.relative_path().to_owned(), index);
        }
    }

    // Capability gate (DESIGN §6.3 `required_capabilities`): resolve the
    // enabled rule set first — select/ignore plus the supersession closure
    // — and only build the fact layers that set can consume. Frontend
    // facts are always built: name resolution and the project index feed
    // every later stage and baseline scene attribution ([`baseline::
    // SceneSpans`] reads the project index only; inline suppressions read
    // no facts at all). The `cost` command computes everything via its own
    // entry point.
    let rules = registry::enabled_rules(&config.select, &config.ignore);
    let mut needs = FactNeeds::for_capabilities(&registry::capability_union(&rules));
    // The coverage summary reads play-duration and builder facts, so it
    // needs the lifecycle interpreter even when no selected rule does.
    needs.lifecycle |= args.analysis_summary;
    let surface = manim_surface(&profile);
    let frontend = frontend::index::analyze(&sources, &config.source_roots, &surface);

    // A whole-project miss falls back to dependency-closed component
    // entries. The frontend remains project-wide; only summaries, Scene
    // lifecycles, cost facts, and diagnostics owned by miss components are
    // recomputed (DESIGN §9 cache-v2).
    let mut recompute_files: BTreeSet<FileId> = sources
        .files()
        .iter()
        .map(crate::source::SourceFile::id)
        .collect();
    let mut cached_diagnostics = Vec::new();
    let mut summary_seed = SummaryTable::default();
    let mut component_plans = Vec::new();
    if cache_key.is_some() {
        recompute_files.clear();
        let dependency_graph = semantic::dependency::SemanticDependencyGraph::from_frontend(
            &sources,
            &config.source_roots,
            &frontend.index,
            &frontend.calls,
        );
        let components = crate::cache::build_analysis_components(&sources, &dependency_graph);
        let project_layout: Vec<&Path> = source_snapshot
            .iter()
            .map(|(path, _)| path.as_path())
            .collect();
        let mut hits = 0;
        for component in components {
            let component_sources: Vec<(&Path, &[u8])> = component
                .files
                .iter()
                .filter_map(|file| {
                    let (path, bytes) = &source_snapshot[file.index()];
                    bytes.as_deref().map(|bytes| (path.as_path(), bytes))
                })
                .collect();
            let key = match crate::cache::build_component_key(
                &project_root,
                &config,
                &profile,
                &project_layout,
                &component_sources,
            ) {
                Ok(key) => Some(key),
                Err(error) => {
                    analysis_cache.disable_with_warning(format!(
                        "incremental cache key could not be built: {error}"
                    ));
                    None
                }
            };
            let dependency_paths = collect_cache_dependency_paths(
                &sources,
                &config,
                &profile,
                &frontend.calls,
                Some(&component.files),
            );
            let cached = key
                .as_ref()
                .and_then(|key| analysis_cache.lookup_component(key));
            let valid_cached = match cached {
                Some(entry) if component_cache_entry_belongs_to(&entry, &component) => Some(entry),
                Some(_) => {
                    if let Some(key) = &key {
                        analysis_cache.reject_component(
                            key,
                            "an incremental entry contained facts owned by another component",
                        );
                    }
                    None
                }
                None => None,
            };
            let hit = if let Some(entry) = valid_cached {
                hits += 1;
                cached_diagnostics.extend(entry.diagnostics);
                summary_seed.summaries.extend(entry.summaries.summaries);
                true
            } else {
                recompute_files.extend(component.files.iter().copied());
                false
            };
            let dependencies_before = if hit || key.is_none() {
                None
            } else {
                match DependencyManifest::capture(&dependency_paths) {
                    Ok(dependencies) => Some(dependencies),
                    Err(error) => {
                        analysis_cache.disable_with_warning(format!(
                            "incremental cache entry was not stored because dependencies could not be stamped: {error}"
                        ));
                        None
                    }
                }
            };
            component_plans.push(ComponentPlan {
                component,
                key,
                hit,
                dependency_paths,
                dependencies_before,
            });
        }
        analysis_cache.record_component_outcome(hits, component_plans.len());
    }

    let recompute_paths: BTreeSet<&str> = recompute_files
        .iter()
        .map(|file| sources.file(*file).relative_path())
        .collect();
    if cache_key.is_some() {
        diagnostics.retain(|diagnostic| recompute_paths.contains(diagnostic.path.as_str()));
    }

    let facts = if recompute_files.is_empty() {
        ProjectFacts {
            index: frontend.index,
            calls: frontend.calls,
            lifecycle: semantic::interpreter::LifecycleFacts::default(),
            cost: cost::CostFacts::default(),
            summaries: summary_seed,
        }
    } else {
        compute_incremental_facts(
            &sources,
            &config,
            &profile,
            needs,
            frontend,
            &recompute_files,
            summary_seed,
        )
    };
    let cache_dependencies = cache_key
        .as_ref()
        .map(|_| collect_cache_dependency_paths(&sources, &config, &profile, &facts.calls, None));
    let cache_dependencies_before = cache_dependencies.as_ref().and_then(|paths| {
        match DependencyManifest::capture(paths) {
            Ok(dependencies) => Some(dependencies),
            Err(error) => {
                analysis_cache.disable_with_warning(format!(
                    "analysis cache entry was not stored because dependencies could not be stamped: {error}"
                ));
                None
            }
        }
    });
    let coverage_report = args.analysis_summary.then(|| {
        coverage::collect(
            &sources,
            &config.target_python,
            &profile,
            &facts.index,
            &facts.calls,
            &facts.lifecycle,
        )
    });
    let ProjectFacts {
        index,
        calls,
        lifecycle,
        cost,
        summaries,
    } = facts;
    let context = RuleContext::new(&sources, &config)
        .with_knowledge(&profile)
        .with_frontend(index, calls)
        .with_lifecycle(lifecycle)
        .with_cost(cost);
    if !recompute_files.is_empty() {
        let rule_diagnostics: Vec<Vec<Diagnostic>> =
            rules.par_iter().map(|rule| rule.run(&context)).collect();
        diagnostics.extend(rule_diagnostics.into_iter().flatten().filter(|diagnostic| {
            cache_key.is_none() || recompute_paths.contains(diagnostic.path.as_str())
        }));
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

    // Store each miss shard before adding hit diagnostics back. Every shard
    // therefore owns exactly its paths and summary FileIds; component
    // validation rejects structurally misplaced cached JSON on lookup.
    let mut component_entries_to_store = Vec::new();
    for plan in &component_plans {
        if plan.hit {
            continue;
        }
        let (Some(key), Some(before)) = (&plan.key, &plan.dependencies_before) else {
            continue;
        };
        let component_diagnostics: Vec<Diagnostic> = diagnostics
            .iter()
            .filter(|diagnostic| plan.component.paths.contains(&diagnostic.path))
            .cloned()
            .collect();
        let component_summaries = SummaryTable {
            summaries: summaries
                .summaries
                .iter()
                .filter(|(_, summary)| {
                    summary
                        .file
                        .is_some_and(|file| plan.component.files.contains(&file))
                })
                .map(|(name, summary)| (name.clone(), summary.clone()))
                .collect(),
        };
        match DependencyManifest::capture(&plan.dependency_paths) {
            Ok(after) if after == *before => component_entries_to_store.push((
                key.clone(),
                ComponentCacheEntry {
                    diagnostics: component_diagnostics,
                    summaries: component_summaries,
                },
                after,
            )),
            Ok(_) => analysis_cache.disable_with_warning(
                "incremental cache entry was not stored because an asset dependency changed during analysis"
                    .to_owned(),
            ),
            Err(error) => analysis_cache.disable_with_warning(format!(
                "incremental cache entry was not stored because dependencies could not be stamped: {error}"
            )),
        }
    }
    analysis_cache.store_components(&component_entries_to_store);

    diagnostics.extend(cached_diagnostics);
    diagnostics.sort_by(Diagnostic::compare_stable);

    if let (Some(key), Some(paths), Some(before)) =
        (&cache_key, &cache_dependencies, &cache_dependencies_before)
    {
        match DependencyManifest::capture(paths) {
            Ok(after) if after == *before => analysis_cache.store(key, &diagnostics, &after),
            Ok(_) => analysis_cache.disable_with_warning(
                "analysis cache entry was not stored because an asset dependency changed during analysis"
                    .to_owned(),
            ),
            Err(error) => analysis_cache.disable_with_warning(format!(
                "analysis cache entry was not stored because dependencies could not be stamped: {error}"
            )),
        }
    }

    // Baseline fingerprints attribute each diagnostic to its enclosing
    // discovered Scene class (DESIGN §8.3: rule ID + relative path +
    // qualified Scene + surrounding token hash).
    let scene_spans = baseline::SceneSpans::build(context.project_index(), &sources);

    // `--write-baseline` records the diagnostics as computed above, before
    // any `--baseline` filtering, so old entries are never dropped when
    // both flags are combined.
    if let Some(path) = &args.write_baseline {
        let document = baseline::render(&diagnostics, &sources, &scene_spans);
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
        diagnostics = known.filter(diagnostics, &sources, &scene_spans);
    }

    let fix_report = if args.fix {
        Some(fixes::apply_with_target(
            &sources,
            &diagnostics,
            args.unsafe_fixes,
            Some(&config.target_python),
        )?)
    } else {
        None
    };

    let profiles = config.active_profile_names();
    let format = resolve_format(args);
    let render_context = RenderContext {
        tool_version: crate::VERSION,
        project_root: ".",
        profiles: &profiles,
        root: &config.project_root,
        files_analyzed: sources.files().len(),
        color: resolve_color(args, format),
    };
    let output = reporting::render(format, &diagnostics, &render_context);

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
        coverage: coverage_report,
        cache_status: analysis_cache.status(),
        cache_warnings: analysis_cache.take_warnings(),
    })
}

/// Finalizes the subset of `check` operations that can be satisfied from a
/// cached diagnostic set. Cache eligibility excludes coverage, baseline, and
/// fixes because those operations need live source/index state.
fn cached_check_report(
    args: &CheckArgs,
    config: ResolvedConfig,
    diagnostics: Vec<Diagnostic>,
    analysis_cache: &mut AnalysisCache,
) -> CheckReport {
    let profiles = config.active_profile_names();
    let format = resolve_format(args);
    let render_context = RenderContext {
        tool_version: crate::VERSION,
        project_root: ".",
        profiles: &profiles,
        root: &config.project_root,
        // A cache hit never rebuilds the file list, so the summary reports
        // the files the cached diagnostics came from rather than inventing a
        // count the run did not observe.
        files_analyzed: cached_file_count(&diagnostics),
        color: resolve_color(args, format),
    };
    let output = reporting::render(format, &diagnostics, &render_context);
    let exit = if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity.reaches(config.fail_level))
    {
        ExitStatus::Failure
    } else {
        ExitStatus::Success
    };
    CheckReport {
        diagnostics,
        output,
        exit,
        config,
        fixes: None,
        coverage: None,
        cache_status: analysis_cache.status(),
        cache_warnings: analysis_cache.take_warnings(),
    }
}

/// Chooses the output format when `--format` was not given.
///
/// `rich` is for a person reading a terminal, so it is the default only when
/// stdout is one. A pipe, a redirect, or CI keeps the stable one-line
/// `concise` output that scripts and the existing tests parse.
fn resolve_format(args: &CheckArgs) -> OutputFormat {
    args.format.unwrap_or_else(|| {
        if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
            OutputFormat::Rich
        } else {
            OutputFormat::Concise
        }
    })
}

/// Decides whether the `rich` renderer emits ANSI styling.
///
/// `--color always` wins over everything, including a redirect, so styled
/// output can be captured deliberately. Otherwise `NO_COLOR` (any value, per
/// the informal standard) disables styling, and `auto` styles only a
/// terminal. No other format is styled.
fn resolve_color(args: &CheckArgs, format: OutputFormat) -> ColorChoice {
    if args.color == ColorMode::Always {
        return ColorChoice::Always;
    }
    if format != OutputFormat::Rich
        || args.color == ColorMode::Never
        || std::env::var_os("NO_COLOR").is_some()
        || !std::io::IsTerminal::is_terminal(&std::io::stdout())
    {
        return ColorChoice::Never;
    }
    ColorChoice::Always
}

/// Files represented in a cached diagnostic set.
fn cached_file_count(diagnostics: &[Diagnostic]) -> usize {
    diagnostics
        .iter()
        .map(|diagnostic| diagnostic.path.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}

/// Reads source bytes once so the cache key and the cold analyzer consume the
/// exact same snapshot. An unreadable file remains a normal per-file MLC000;
/// that run simply bypasses caching because its read error is environment
/// state rather than stable source content.
fn read_source_snapshot(files: &[PathBuf]) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    files
        .iter()
        .map(|path| (path.clone(), read_admissible_source(path)))
        .collect()
}

/// Reads a file only if it passes the same admission checks the source loader
/// applies. Reading a FIFO blocks forever and a character device never ends,
/// so both are refused on their metadata before any read starts; the loader
/// then records the refusal as `MLC000`.
fn read_admissible_source(path: &Path) -> Option<Vec<u8>> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || crate::source_limits::check_source_length(metadata.len()).is_err() {
        return None;
    }
    std::fs::read(path).ok()
}

/// Operations that need source/index state after diagnostics are produced do
/// a full analysis in cache v2. Normal checks, every output format, and
/// `--statistics` remain cacheable.
fn cache_eligible(args: &CheckArgs) -> bool {
    !args.no_cache
        && !args.fix
        && args.baseline.is_none()
        && args.write_baseline.is_none()
        && !args.analysis_summary
}

fn component_cache_entry_belongs_to(
    entry: &ComponentCacheEntry,
    component: &AnalysisComponent,
) -> bool {
    entry
        .diagnostics
        .iter()
        .all(|diagnostic| component.paths.contains(&diagnostic.path))
        && entry.summaries.belongs_to_files(&component.files)
}

const CACHE_SVG_MOBJECT: &str = "manim.mobject.svg.svg_mobject.SVGMobject";
const CACHE_IMAGE_MOBJECT: &str = "manim.mobject.types.image_mobject.ImageMobject";
const CACHE_SVG_EXTENSIONS: &[&str] = &[".svg"];
const CACHE_RASTER_EXTENSIONS: &[&str] = &[".jpg", ".jpeg", ".png", ".gif", ".ico"];

/// Collects every filesystem path whose state can change the asset rules
/// without changing Python source. Exact candidates cover existence/content;
/// case-insensitive component walks add the directory listings consulted by
/// MLR104/MLD305. The resulting manifest is validated before every hit.
fn collect_cache_dependency_paths(
    sources: &SourceManager,
    config: &ResolvedConfig,
    profile: &KnowledgeProfile,
    calls: &frontend::index::QualifiedCallFacts,
    included_files: Option<&BTreeSet<FileId>>,
) -> BTreeSet<PathBuf> {
    let mut dependencies = BTreeSet::new();
    for call in &calls.calls {
        if included_files.is_some_and(|files| !files.contains(&call.file)) {
            continue;
        }
        let known: Vec<&str> = call
            .candidates
            .iter()
            .filter(|candidate| profile.symbol(candidate).is_some())
            .map(String::as_str)
            .collect();
        let [constructor] = known.as_slice() else {
            continue;
        };
        let (parameter, extensions) = match *constructor {
            CACHE_SVG_MOBJECT => ("file_name", CACHE_SVG_EXTENSIONS),
            CACHE_IMAGE_MOBJECT => ("filename_or_array", CACHE_RASTER_EXTENSIONS),
            _ => continue,
        };
        let Some(argument) = call.keyword(parameter).or_else(|| call.positional(0)) else {
            continue;
        };
        let Some(LiteralFact::Str { value, prefix, .. }) = &argument.literal else {
            continue;
        };
        if prefix.bytes || value.is_empty() || value.starts_with('~') {
            continue;
        }
        let foreign_path = value.contains('\\') || cache_has_drive_prefix(value);
        let literal_path = Path::new(value);
        for render_profile in &config.active_profiles {
            let working_dir = sources
                .project_root()
                .join(&render_profile.working_directory);
            add_path_to_existing_ancestor(&mut dependencies, &working_dir);
            if foreign_path {
                continue;
            }
            let assets_base = working_dir.join(&render_profile.assets_dir);
            add_path_to_existing_ancestor(&mut dependencies, &assets_base);
            let mut candidates = vec![working_dir.join(value), assets_base.join(value)];
            candidates.extend(
                extensions
                    .iter()
                    .map(|extension| assets_base.join(format!("{value}{extension}"))),
            );
            for candidate in candidates {
                add_path_to_existing_ancestor(&mut dependencies, &candidate);
            }
            if !literal_path.is_absolute() {
                add_case_walk(&mut dependencies, &working_dir, value);
                add_case_walk(&mut dependencies, &assets_base, value);
                for extension in extensions {
                    add_case_walk(
                        &mut dependencies,
                        &assets_base,
                        &format!("{value}{extension}"),
                    );
                }
                if let Some(source_dir) = sources.file(call.file).path().parent() {
                    add_path_to_existing_ancestor(&mut dependencies, &source_dir.join(value));
                }
            }
        }
    }
    dependencies
}

/// Adds a path, each missing exact ancestor, and the first existing ancestor.
/// This makes creation/deletion invalidate a cached missing-path verdict.
fn add_path_to_existing_ancestor(paths: &mut BTreeSet<PathBuf>, path: &Path) {
    let mut current = Some(path);
    while let Some(candidate) = current {
        paths.insert(candidate.to_path_buf());
        if candidate.exists() {
            break;
        }
        current = candidate.parent();
    }
}

/// Mirrors the asset rules' component-wise case scan while recording every
/// directory whose entries influenced the result, including a partially
/// matched differently-cased prefix.
fn add_case_walk(paths: &mut BTreeSet<PathBuf>, base: &Path, relative: &str) {
    let mut current = base.to_path_buf();
    paths.insert(current.clone());
    for component in Path::new(relative).components() {
        let Component::Normal(part) = component else {
            return;
        };
        let exact = current.join(part);
        paths.insert(exact.clone());
        if exact.exists() {
            current = exact;
            continue;
        }
        let Some(part) = part.to_str() else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&current) else {
            return;
        };
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        names.sort();
        let Some(matched) = names
            .into_iter()
            .find(|name| name.to_lowercase() == part.to_lowercase())
        else {
            return;
        };
        current = current.join(matched);
        paths.insert(current.clone());
    }
}

fn cache_has_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Runs `manim-lint coverage [PATH...] [--format text|json]`: the
/// analysis-coverage report as a standalone command on stdout.
///
/// Computes the same fact stack a `check` run would (frontend facts plus
/// the lifecycle interpreter; the cost model is not needed) and renders
/// the counters of everything the analysis could not resolve. Always
/// exits 0 unless configuration or IO fails (exit 2).
pub fn run_coverage(
    paths: &[PathBuf],
    format: CoverageFormat,
) -> Result<Execution, ApplicationError> {
    let args = CheckArgs {
        paths: paths.to_vec(),
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
    validate_declared_manim_version(&config, &profile)?;
    let files = collect_python_files(&paths, &project_root, &config.exclude)?;
    let mut sources = SourceManager::new(project_root);
    for file in &files {
        sources.load_file(file);
    }
    let facts = compute_facts(
        &sources,
        &config,
        &profile,
        FactNeeds {
            lifecycle: true,
            cost: false,
        },
    );
    let report = coverage::collect(
        &sources,
        &config.target_python,
        &profile,
        &facts.index,
        &facts.calls,
        &facts.lifecycle,
    );
    let output = match format {
        CoverageFormat::Text => coverage::render_text(&report),
        CoverageFormat::Json => coverage::render_json(&report),
    };
    Ok(Execution::success(output))
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
    summaries: SummaryTable,
}

/// Which optional fact layers a run must compute. Frontend facts (project
/// index, qualified calls, statement and binding facts) are always built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FactNeeds {
    /// Run the lifecycle abstract interpreter.
    lifecycle: bool,
    /// Run the symbolic cost model (implies `lifecycle`: cost facts are
    /// computed over the lifecycle scenes).
    cost: bool,
}

impl FactNeeds {
    /// Every fact layer (the `cost` command and tests use this).
    const ALL: Self = Self {
        lifecycle: true,
        cost: true,
    };

    /// The layers implied by a capability union: `"lifecycle"` needs the
    /// interpreter, `"cost-facts"` needs the cost model *and* the
    /// interpreter it consumes, and `"cost-report"` (the opt-in `MLP225`
    /// capability whose home is the `cost` command) needs the same stack
    /// when a `--select MLP225` check run opts in. `"source"` /
    /// `"qualified-calls"` need nothing beyond the always-built frontend
    /// facts; `"local-fork-overlay"` is a property of the loaded knowledge
    /// profile, not a fact layer.
    fn for_capabilities(capabilities: &BTreeSet<&'static str>) -> Self {
        let cost = capabilities.contains("cost-facts") || capabilities.contains("cost-report");
        Self {
            lifecycle: cost || capabilities.contains("lifecycle"),
            cost,
        }
    }
}

/// Runs the fact-layer pipeline (DESIGN §5.1): frontend facts, then — when
/// `needs` asks for them — the lifecycle abstract interpreter over the
/// discovered scenes and the symbolic cost model. A skipped layer keeps
/// its empty default, which the [`RuleContext`] contract reads as "not
/// analyzed": rules consuming it stay silent, exactly what the select
/// filter would enforce afterwards anyway. Rules only ever see the result.
fn compute_facts(
    sources: &SourceManager,
    config: &ResolvedConfig,
    profile: &KnowledgeProfile,
    needs: FactNeeds,
) -> ProjectFacts {
    let surface = manim_surface(profile);
    let facts = frontend::index::analyze(sources, &config.source_roots, &surface);
    let lifecycle = if needs.lifecycle {
        semantic::interpreter::analyze(sources, &facts.index, &facts.calls, Some(profile))
    } else {
        semantic::interpreter::LifecycleFacts::default()
    };
    let cost = if needs.cost {
        cost::CostFacts::compute_with_lifecycle(
            sources,
            &facts.index,
            &facts.calls,
            Some(profile),
            &config.active_profiles,
            &lifecycle,
        )
    } else {
        cost::CostFacts::default()
    };
    ProjectFacts {
        index: facts.index,
        calls: facts.calls,
        lifecycle,
        cost,
        summaries: SummaryTable::default(),
    }
}

/// Computes only the derived facts owned by cache-miss components. Frontend
/// facts are deliberately project-wide and supplied by the caller; cached
/// method summaries seed dependency components that were validated as hits.
fn compute_incremental_facts(
    sources: &SourceManager,
    config: &ResolvedConfig,
    profile: &KnowledgeProfile,
    needs: FactNeeds,
    frontend: frontend::index::FrontendFacts,
    recompute_files: &BTreeSet<FileId>,
    summary_seed: SummaryTable,
) -> ProjectFacts {
    let (lifecycle, summaries) = if needs.lifecycle {
        semantic::interpreter::analyze_incremental(
            sources,
            &frontend.index,
            &frontend.calls,
            Some(profile),
            recompute_files,
            summary_seed,
        )
    } else {
        (
            semantic::interpreter::LifecycleFacts::default(),
            summary_seed,
        )
    };
    let cost = if needs.cost {
        cost::CostFacts::compute_with_lifecycle(
            sources,
            &frontend.index,
            &frontend.calls,
            Some(profile),
            &config.active_profiles,
            &lifecycle,
        )
    } else {
        cost::CostFacts::default()
    };
    ProjectFacts {
        index: frontend.index,
        calls: frontend.calls,
        lifecycle,
        cost,
        summaries,
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
    validate_declared_manim_version(&config, &profile)?;
    let files = collect_python_files(&paths, &project_root, &config.exclude)?;
    let mut sources = SourceManager::new(project_root);
    for file in &files {
        sources.load_file(file);
    }
    // The cost report reads lifecycle plays and cost contexts directly:
    // always compute every layer here.
    let facts = compute_facts(&sources, &config, &profile, FactNeeds::ALL);
    let output = render_cost_report(&sources, &config, &profile, &facts, scene_filter)?;
    Ok(Execution::success(output))
}

/// Renders the per-scene cost report over the computed fact stack.
fn render_cost_report(
    sources: &SourceManager,
    config: &ResolvedConfig,
    knowledge: &KnowledgeProfile,
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
    // Fork fast-path verdicts (DESIGN §7.3 MLP225): evaluated only when
    // the loaded knowledge profile declares fork capabilities — the whole
    // section is absent under upstream profiles.
    let fork_paths = cost::fork::evaluate(
        &facts.lifecycle,
        &facts.calls,
        Some(knowledge),
        &config.active_profiles,
    );
    for scene in scenes {
        render_scene_cost(&mut output, sources, config, facts, scene);
        if let Some(scene_paths) = fork_paths
            .scenes
            .iter()
            .find(|paths| paths.scene == scene.qualified_name)
        {
            render_fork_paths(&mut output, sources, knowledge, scene_paths);
        }
    }
    Ok(output)
}

/// Renders one scene's fork fast-path section (DESIGN §7.3 `MLP225`): per
/// play, either no detected blocker or the causal chain that closes the
/// fast path — with the measured calibration evidence where the DESIGN
/// quotes it, and never advice to remove a feature.
fn render_fork_paths(
    output: &mut String,
    sources: &SourceManager,
    knowledge: &KnowledgeProfile,
    scene_paths: &cost::fork::SceneForkPaths,
) {
    use cost::fork::{GateEvaluation, GateKind};

    let locate = |site: crate::semantic::values::AllocationSite| {
        cost_location(sources, site.file, site.start)
    };
    for profile_paths in &scene_paths.profiles {
        let _ = writeln!(
            output,
            "  fork fast paths (profile {profile}, knowledge {knowledge}):",
            profile = profile_paths.profile,
            knowledge = knowledge.name,
        );
        let gates: [(GateKind, &GateEvaluation, String); 3] = [
            (
                GateKind::ForkPerPlay,
                &profile_paths.fork,
                format!("cairo_fork_workers {}", profile_paths.fork_workers),
            ),
            (
                GateKind::StaticLayers,
                &profile_paths.static_layers,
                format!(
                    "cairo_static_layers {}",
                    if profile_paths.static_layers_requested {
                        "on"
                    } else {
                        "off"
                    }
                ),
            ),
            (
                GateKind::BulkInterpolation,
                &profile_paths.bulk,
                String::new(),
            ),
        ];
        let mut any_loss = false;
        for (gate, evaluation, config_note) in &gates {
            let label = gate.label();
            let suffix = if config_note.is_empty() {
                String::new()
            } else {
                format!(" ({config_note})")
            };
            match evaluation {
                GateEvaluation::NotDeclared => {}
                GateEvaluation::Unrequested { reason } => {
                    let _ = writeln!(
                        output,
                        "    {label}: not requested ({reason}); nothing to report"
                    );
                }
                GateEvaluation::Plays(outcomes) => {
                    let _ = writeln!(output, "    {label}{suffix}:");
                    for outcome in outcomes {
                        let _ = writeln!(
                            output,
                            "      {location} play #{ordinal}: {description}",
                            location = locate(outcome.site),
                            ordinal = outcome.ordinal,
                            description = cost::fork::describe_outcome(*gate, outcome, &locate),
                        );
                    }
                    if evaluation.has_loss() {
                        any_loss = true;
                        if let Some(evidence) = gate.measured_evidence() {
                            let _ = writeln!(output, "      evidence: {evidence}");
                        }
                    }
                }
            }
        }
        if any_loss {
            let _ = writeln!(
                output,
                "    note: the features named above can be correct expression; this \
                 section explains the render-path consequence and never advises \
                 removing them"
            );
        }
    }
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
    use crate::rules::performance::support::{display_frames, display_seconds};
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
    // chain, non-neutral multiplicity factors, and the per-callback
    // liveness note — which plays provably execute the callback per frame
    // (DESIGN §3.2 suspension, §3.3 wait dynamics). "none" means the
    // callback provably never runs per frame in the analyzed plays.
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
            let liveness_note = facts.cost.context_liveness(context).map_or_else(
                || "unknown".to_owned(),
                |liveness| {
                    if !liveness.resolved {
                        return "unknown".to_owned();
                    }
                    let proven: Vec<String> = liveness
                        .proven()
                        .map(|play| cost_location(sources, play.site.file, play.site.start))
                        .collect();
                    if !proven.is_empty() {
                        proven.join(", ")
                    } else if liveness.maybe().next().is_some() {
                        "none (execution possible but unproven)".to_owned()
                    } else {
                        "none".to_owned()
                    }
                },
            );
            let line = format!(
                "    {location} entry {entry}; path {path}; factors {factors}; \
                 proven execution plays: {liveness_note}",
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

    // Per-frame constructions reachable from those contexts. Quantities
    // sum over the proven execution plays only (DESIGN §15 invariant 9);
    // a callback that provably never runs per frame says so instead of
    // fabricating an invocation count.
    let _ = writeln!(output, "  per-frame constructions:");
    let mut construction_rows = 0_usize;
    for construction in facts.cost.constructions_in_hot_contexts() {
        let call = &facts.calls.calls[construction.call_index];
        if call.context.class_name.as_deref() != Some(scene.qualified_name.as_str()) {
            continue;
        }
        construction_rows += 1;
        let execution = facts.cost.call_execution(construction.call_index);
        let row = if execution.has_proven() {
            let invocations = facts
                .cost
                .proven_frames_for_call(construction.call_index)
                .map_or_else(
                    || {
                        format!(
                            "x per-frame across {} proven play(s)",
                            execution.proven.len()
                        )
                    },
                    |frames| {
                        format!(
                            "x {} invocations across {} proven play(s)",
                            display_frames(&frames),
                            execution.proven.len()
                        )
                    },
                );
            format!("construction {invocations}")
        } else if execution.possibly_executes() {
            "construction x per-frame (execution not proven)".to_owned()
        } else {
            "construction: no proven per-frame execution".to_owned()
        };
        let _ = writeln!(
            output,
            "    {location} {class} {row}",
            location = cost_location(sources, call.file, u32::from(call.call_range.start())),
            class = short_class_name(&construction.symbol),
        );
    }
    if construction_rows == 0 {
        let _ = writeln!(output, "    (none)");
    }

    // Frame-varying resource keys: distinct Text/TeX/SVG cache keys grow
    // with the frame count (`K_resource ≈ F`) — but only over the plays
    // where the callback provably executes.
    let _ = writeln!(output, "  resource-key growth:");
    let mut resource_rows = 0_usize;
    for fact in facts.cost.frame_varying_resource_keys() {
        let call = &facts.calls.calls[fact.call_index];
        if call.context.class_name.as_deref() != Some(scene.qualified_name.as_str()) {
            continue;
        }
        resource_rows += 1;
        let execution = facts.cost.call_execution(fact.call_index);
        let keys = if execution.has_proven() {
            facts
                .cost
                .proven_frames_for_call(fact.call_index)
                .map_or_else(
                    || {
                        format!(
                            "one per rendered frame of {} proven play(s)",
                            execution.proven.len()
                        )
                    },
                    |frames| {
                        format!(
                            "{} across {} proven play(s)",
                            display_frames(&frames),
                            execution.proven.len()
                        )
                    },
                )
        } else if execution.possibly_executes() {
            "one per rendered frame (execution not proven)".to_owned()
        } else {
            "no proven per-frame execution".to_owned()
        };
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

/// DESIGN §8.2 honesty check: a `manim-version` declared in configuration
/// must fall inside the loaded knowledge profile's supported Manim range
/// (exit 2 otherwise). An absent declaration is not validated — the
/// builtin default is informational only.
fn validate_declared_manim_version(
    config: &ResolvedConfig,
    profile: &KnowledgeProfile,
) -> Result<(), ApplicationError> {
    if let Some(declared) = &config.declared_manim_version {
        loader::validate_manim_version(declared, &profile.name, &profile.manim_version)?;
    }
    Ok(())
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
    let mut visited = BTreeSet::new();
    for path in paths {
        if path.is_file() {
            files.push(path.clone());
        } else {
            walk_directory(
                path,
                project_root,
                &exclude_files,
                &exclude_dirs,
                &mut visited,
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
    visited: &mut BTreeSet<PathBuf>,
    files: &mut Vec<PathBuf>,
) -> Result<(), ApplicationError> {
    // A symlink cycle (a directory link pointing at an ancestor) would
    // otherwise recurse forever: every directory is entered once by its
    // canonical identity. Deterministic order is preserved — entries stay
    // sorted and a revisited directory is simply skipped.
    let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    if !visited.insert(canonical) {
        return Ok(());
    }
    // Per-entry read errors surface as ApplicationError (exit 2), exactly
    // like a directory that cannot be opened at all: silently skipping an
    // unreadable entry would claim "checked" for files never seen.
    let mut entries: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|source| ApplicationError::Io {
        path: dir.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| ApplicationError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        entries.push(entry.path());
    }
    entries.sort();

    for entry in entries {
        // A symlink is followed only while it stays inside the project. An
        // analyzed project can come from an untrusted checkout, and `--fix`
        // writes back to the discovered path: without this, a link committed
        // to a repository would let `manim-lint check --fix` rewrite an
        // arbitrary file outside it.
        if !path_is_inside(project_root, &entry) {
            continue;
        }
        let relative = crate::source::relative_posix_path(project_root, &entry);
        if entry.is_dir() {
            let name = entry
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if name == ".git" || name == "__pycache__" || exclude_dirs.is_match(&relative) {
                continue;
            }
            walk_directory(
                &entry,
                project_root,
                exclude_files,
                exclude_dirs,
                visited,
                files,
            )?;
        } else if entry.extension().is_some_and(|extension| extension == "py")
            && !exclude_files.is_match(&relative)
        {
            files.push(entry);
        }
    }
    Ok(())
}

/// Whether `path` resolves inside `project_root`.
///
/// Both sides are canonicalized so a symlink is judged by where it actually
/// leads, not by how it is spelled. A path that cannot be canonicalized (it
/// was removed between listing and this check, or a component is unreadable)
/// is treated as outside: refusing to analyze it is the conservative answer.
pub(crate) fn path_is_inside(project_root: &Path, path: &Path) -> bool {
    let Ok(root) = project_root.canonicalize() else {
        return false;
    };
    let Ok(resolved) = path.canonicalize() else {
        return false;
    };
    resolved.starts_with(&root)
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

#[allow(
    clippy::too_many_lines,
    reason = "the deterministic embedded rule-document table has one row per implemented rule"
)]
fn run_explain(rule: &str) -> Result<Execution, ApplicationError> {
    /// Embedded rule documentation for implemented rules.
    const DOCS: [(&str, &str); 92] = [
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
        ("MLC114", include_str!("../docs/rules/MLC114.md")),
        ("MLC115", include_str!("../docs/rules/MLC115.md")),
        ("MLC116", include_str!("../docs/rules/MLC116.md")),
        ("MLC117", include_str!("../docs/rules/MLC117.md")),
        ("MLC118", include_str!("../docs/rules/MLC118.md")),
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
        ("MLR109", include_str!("../docs/rules/MLR109.md")),
        ("MLR110", include_str!("../docs/rules/MLR110.md")),
        ("MLR111", include_str!("../docs/rules/MLR111.md")),
        ("MLR112", include_str!("../docs/rules/MLR112.md")),
        ("MLR113", include_str!("../docs/rules/MLR113.md")),
        ("MLR114", include_str!("../docs/rules/MLR114.md")),
        ("MLR115", include_str!("../docs/rules/MLR115.md")),
        ("MLR116", include_str!("../docs/rules/MLR116.md")),
        ("MLR117", include_str!("../docs/rules/MLR117.md")),
        ("MLR118", include_str!("../docs/rules/MLR118.md")),
        ("MLR119", include_str!("../docs/rules/MLR119.md")),
        ("MLR120", include_str!("../docs/rules/MLR120.md")),
        ("MLR121", include_str!("../docs/rules/MLR121.md")),
        ("MLR122", include_str!("../docs/rules/MLR122.md")),
        ("MLR123", include_str!("../docs/rules/MLR123.md")),
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
        ("MLP212", include_str!("../docs/rules/MLP212.md")),
        ("MLP213", include_str!("../docs/rules/MLP213.md")),
        ("MLP214", include_str!("../docs/rules/MLP214.md")),
        ("MLP215", include_str!("../docs/rules/MLP215.md")),
        ("MLP216", include_str!("../docs/rules/MLP216.md")),
        ("MLP217", include_str!("../docs/rules/MLP217.md")),
        ("MLP218", include_str!("../docs/rules/MLP218.md")),
        ("MLP219", include_str!("../docs/rules/MLP219.md")),
        ("MLP220", include_str!("../docs/rules/MLP220.md")),
        ("MLP221", include_str!("../docs/rules/MLP221.md")),
        ("MLP222", include_str!("../docs/rules/MLP222.md")),
        ("MLP223", include_str!("../docs/rules/MLP223.md")),
        ("MLP224", include_str!("../docs/rules/MLP224.md")),
        ("MLP225", include_str!("../docs/rules/MLP225.md")),
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
    run_config_at(&current_dir)
}

/// Runs `manim-lint config` for the project containing `start`.
///
/// Prints the resolved configuration as one JSON object, extended with an
/// `enforcement` section that states which settings are actually enforced
/// and which are informational (DESIGN §8.2 honesty): a setting the run
/// cannot honor must never be echoed back as if it were consulted.
pub fn run_config_at(start: &Path) -> Result<Execution, ApplicationError> {
    let args = CheckArgs::default();
    let config = resolve_config(&args, &discover_project_root(&[start.to_path_buf()])?)?;
    let profile_name = config
        .knowledge_profile
        .clone()
        .unwrap_or_else(|| knowledge::DEFAULT_PROFILE.to_owned());
    let profile = knowledge::load(&profile_name)?;
    validate_declared_manim_version(&config, &profile)?;

    let manim_version_note = config.declared_manim_version.as_ref().map_or_else(
        || {
            format!(
                "not declared; the default \"{version}\" is informational and \
                 not validated (knowledge profile {name} supports Manim \
                 {range})",
                version = config.manim_version,
                name = profile.name,
                range = profile.manim_version,
            )
        },
        |declared| {
            format!(
                "enforced: declared \"{declared}\" is validated against \
                 knowledge profile {name} (supported Manim range {range})",
                name = profile.name,
                range = profile.manim_version,
            )
        },
    );
    let mut value =
        serde_json::to_value(&config).map_err(|error| ApplicationError::Cli(error.to_string()))?;
    value["enforcement"] = serde_json::json!({
        "manim-version": manim_version_note,
        "target-python": format!(
            "enforced post-parse: validated for format and parser support \
             (Python {min_major}.{min_minor} through \
             {max_major}.{max_minor}); the bundled parser grammar is fixed \
             (rustpython-parser 0.4, Python {max_major}.{max_minor} \
             grammar, no feature_version pinning), so target-python does \
             not change parsing, but a parsed construct newer than the \
             target (match, except*, PEP 695, walrus, positional-only \
             parameters) is reported as MLC000 and a --fix that introduces \
             one is rolled back",
            min_major = loader::MIN_TARGET_PYTHON.0,
            min_minor = loader::MIN_TARGET_PYTHON.1,
            max_major = loader::MAX_TARGET_PYTHON.0,
            max_minor = loader::MAX_TARGET_PYTHON.1,
        ),
        "stub-paths": "not implemented yet; a non-empty stub-paths is a \
                       configuration error",
        "frame-rate": "enforced: must be a positive finite number in every \
                       analyzed profile, from any source (CLI, profile, \
                       manim.cfg)",
        "resolution": "enforced: pixel width and height must be nonzero in \
                       every analyzed profile, from any source (CLI, \
                       profile, manim.cfg)",
        "knowledge-profile": format!(
            "enforced: profile {name} is loaded; an unknown \
             knowledge-profile is a configuration error",
            name = profile.name,
        ),
    });
    let mut output = serde_json::to_string_pretty(&value)
        .map_err(|error| ApplicationError::Cli(error.to_string()))?;
    output.push('\n');
    Ok(Execution::success(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::{Confidence, Severity, SourcePosition, SourceSpan};

    /// Capability → fact-layer mapping (DESIGN §6.3): only `lifecycle`
    /// and `cost-facts` demand layers beyond the always-built frontend
    /// facts, and cost facts imply the lifecycle interpreter they are
    /// computed over.
    #[test]
    fn fact_needs_follow_the_capability_union() {
        let needs = |capabilities: &[&'static str]| {
            FactNeeds::for_capabilities(&capabilities.iter().copied().collect())
        };
        assert_eq!(
            needs(&[]),
            FactNeeds {
                lifecycle: false,
                cost: false
            }
        );
        assert_eq!(
            needs(&["source", "qualified-calls"]),
            FactNeeds {
                lifecycle: false,
                cost: false
            }
        );
        assert_eq!(
            needs(&["qualified-calls", "lifecycle"]),
            FactNeeds {
                lifecycle: true,
                cost: false
            }
        );
        // Cost facts are computed over the lifecycle scenes: requesting
        // them always runs the interpreter too.
        assert_eq!(needs(&["cost-facts"]), FactNeeds::ALL);
        assert_eq!(needs(&["qualified-calls", "cost-facts"]), FactNeeds::ALL);
        // The MLP225 opt-in capabilities: `cost-report` needs the full
        // stack (an explicit `--select MLP225` must actually evaluate);
        // `local-fork-overlay` alone is a knowledge-profile property,
        // not a fact layer.
        assert_eq!(
            needs(&["cost-report", "local-fork-overlay"]),
            FactNeeds::ALL
        );
        assert_eq!(
            needs(&["local-fork-overlay"]),
            FactNeeds {
                lifecycle: false,
                cost: false
            }
        );
    }

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
