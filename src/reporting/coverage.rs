//! Analysis-coverage reporting (the reviewer's "trust feature").
//!
//! The analyzer's conservative `Unknown` silences are correct but
//! invisible: a clean `check` run cannot be distinguished from a run that
//! silently failed to resolve half the project. This module counts, from
//! the already-computed fact surfaces, everything the analyzer could NOT
//! resolve — unresolved imports, calls with empty candidate sets, unknown
//! play durations, `.animate` builders with untracked targets, Python
//! constructs above `target-python`, resolved `manim.*` candidates absent
//! from the knowledge profile, and scenes whose constructor state is
//! unknown — and renders it as humane text or a stable JSON document.
//!
//! Every number is a count of facts. The only ratios are simple
//! `resolved / total` count pairs, printed as such (DESIGN §15 invariant
//! 9: no fabricated quantities). Output is deterministic and byte-stable
//! for identical inputs.
//!
//! Consumed by `qual coverage [PATH...]` (stdout) and
//! `qual check --analysis-summary` (stderr, after diagnostics;
//! never affects stdout or the exit code).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use rustpython_parser::ast;
use serde::Serialize;

use crate::frontend::features;
use crate::frontend::imports::{ImportTarget, ImportedNames, import_from_names};
use crate::frontend::index::{ProjectIndex, QualifiedCallFacts};
use crate::knowledge::KnowledgeProfile;
use crate::semantic::interpreter::{LifecycleFacts, SceneLifecycle};
use crate::semantic::values::Num;
use crate::source::{SourceFile, SourceManager};

/// Output format of the `coverage` subcommand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CoverageFormat {
    /// Grouped human-readable text.
    #[default]
    Text,
    /// The JSON document described in the README (stable keys).
    Json,
}

impl std::str::FromStr for CoverageFormat {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "text" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            _ => Err(format!("unknown format: {value} (expected text|json)")),
        }
    }
}

/// Unresolved-analysis counters for one analyzed file.
#[derive(Debug, Clone, Serialize)]
pub struct FileCoverage {
    /// Project-relative POSIX path.
    pub path: String,
    /// Whether the file decoded and parsed.
    pub parsed: bool,
    /// Parsed constructs above the configured `target-python` (the
    /// already-emitted MLC000 syntax-gate diagnostics).
    pub gated_constructs: usize,
    /// `from X import *` statements whose source module the analyzer
    /// cannot enumerate (external packages, `manim.*` submodules,
    /// unresolvable relative stars): the namespace is incomplete.
    pub unresolved_star_imports: usize,
    /// Imported names from relative imports that escape the project tree;
    /// each binds to an explicit Unknown.
    pub unresolved_relative_imports: usize,
    /// Call sites in this file.
    pub calls: usize,
    /// Call sites whose candidate target set is empty: no rule can reason
    /// about them.
    pub unresolved_calls: usize,
    /// Callee spellings of the unresolved calls, with occurrence counts.
    pub unresolved_call_names: BTreeMap<String, usize>,
    /// Resolved `manim.*` candidates that neither the knowledge profile
    /// nor its curated base chains describe: the call resolves, but its
    /// effects are unknown to every rule.
    pub apis_not_in_profile: BTreeSet<String>,
}

impl FileCoverage {
    /// Whether anything in this file limited the analysis.
    #[must_use]
    pub fn has_findings(&self) -> bool {
        !self.parsed
            || self.gated_constructs > 0
            || self.unresolved_star_imports > 0
            || self.unresolved_relative_imports > 0
            || self.unresolved_calls > 0
            || !self.apis_not_in_profile.is_empty()
    }
}

/// Unresolved-analysis counters for one discovered Scene class.
#[derive(Debug, Clone, Serialize)]
pub struct SceneCoverage {
    /// Qualified Scene class name.
    pub name: String,
    /// Project-relative path of the defining file.
    pub path: String,
    /// The MRO could not be linearized: the lifecycle interpreter ran
    /// with unknown constructor state and lifecycle rules stayed
    /// conservative for this scene.
    pub constructor_state_unknown: bool,
    /// `play` / `wait` groups traced for this scene.
    pub plays: usize,
    /// Plays whose duration is Unknown (no literal `run_time` reachable):
    /// frame counts and cost bounds stay unquantified for them.
    pub plays_with_unknown_duration: usize,
    /// `.animate` builders created in this scene.
    pub builders: usize,
    /// `.animate` builders whose target object identity was not tracked:
    /// staleness and channel rules stayed silent for them.
    pub builders_with_unknown_target: usize,
    /// Helper calls that fell back to an effect summary instead of being
    /// inlined (recursion cycles / depth cap / unresolvable callees).
    /// Absent: the fact layer records the frontier project-wide only
    /// (see [`helper_inline_fallbacks`]), so the count lives on
    /// [`ProjectCoverage`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub helper_inline_fallbacks: Option<usize>,
}

/// One aggregated unresolved callee spelling.
#[derive(Debug, Clone, Serialize)]
pub struct NameCount {
    /// Callee as written (dotted), or `<dynamic>`.
    pub name: String,
    /// Occurrences across the project.
    pub count: usize,
}

/// Project-wide totals over the per-file and per-scene counters.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectCoverage {
    /// Files selected for analysis.
    pub files: usize,
    /// Files that decoded and parsed.
    pub files_parsed: usize,
    /// Total gated constructs (MLC000 syntax gate).
    pub gated_constructs: usize,
    /// Total unresolved star imports.
    pub unresolved_star_imports: usize,
    /// Total unresolved relative-import names.
    pub unresolved_relative_imports: usize,
    /// Total call sites.
    pub calls: usize,
    /// Total call sites with empty candidate sets.
    pub unresolved_calls: usize,
    /// Most frequent unresolved callee spellings (count-descending, then
    /// name; at most five).
    pub top_unresolved_call_names: Vec<NameCount>,
    /// Total traced plays.
    pub plays: usize,
    /// Total plays with Unknown duration.
    pub plays_with_unknown_duration: usize,
    /// Discovered Scene classes.
    pub scenes: usize,
    /// Scenes with unknown constructor state.
    pub scenes_with_unknown_constructor_state: usize,
    /// Total `.animate` builders.
    pub builders: usize,
    /// Total builders with untracked targets.
    pub builders_with_unknown_target: usize,
    /// Union of the per-file `apis_not_in_profile` sets.
    pub apis_not_in_profile: BTreeSet<String>,
    /// Helper call sites that fell back to an effect summary instead of
    /// being inlined ([`LifecycleFacts::inline_fallbacks`]),
    /// deduplicated across scenes sharing a helper chain.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub helper_inline_fallbacks: Option<usize>,
}

/// The full analysis-coverage report.
#[derive(Debug, Clone, Serialize)]
pub struct CoverageReport {
    /// Loaded knowledge profile name.
    pub knowledge_profile: String,
    /// Configured `target-python`.
    pub target_python: String,
    /// Per-file counters, in deterministic file order.
    pub files: Vec<FileCoverage>,
    /// Per-scene counters, sorted by qualified scene name.
    pub scenes: Vec<SceneCoverage>,
    /// Project totals.
    pub project: ProjectCoverage,
}

/// Per-scene helper-inline-fallback count.
///
/// [`LifecycleFacts::inline_fallbacks`] records the helper call sites
/// that fell back to an effect summary instead of being inlined — but
/// project-wide, deduplicated across scenes that share a helper chain,
/// so no sound per-scene attribution exists. The per-scene row therefore
/// stays absent (a split would fabricate attribution) while the project
/// total reports the real frontier count.
#[allow(
    clippy::unnecessary_wraps,
    reason = "signature is the stable hookup point should the fact gain a per-scene shape"
)]
fn helper_inline_fallbacks(_lifecycle: &LifecycleFacts, _scene: &SceneLifecycle) -> Option<usize> {
    None
}

/// Computes the coverage report from the already-built fact surfaces.
///
/// Reads only: the loaded sources (parse status, syntax-gate walk), the
/// project index (module tree and identities), the qualified call facts,
/// the lifecycle facts, and the knowledge profile. Nothing is re-analyzed
/// and no fact layer is mutated.
#[must_use]
pub fn collect(
    sources: &SourceManager,
    target_python: &str,
    profile: &KnowledgeProfile,
    index: &ProjectIndex,
    calls: &QualifiedCallFacts,
    lifecycle: &LifecycleFacts,
) -> CoverageReport {
    let mut files = Vec::new();
    for file in sources.files() {
        files.push(file_coverage(file, target_python, profile, index, calls));
    }

    let mut scenes = Vec::new();
    for scene in &lifecycle.scenes {
        let plays_unknown = scene
            .plays
            .iter()
            .filter(|play| matches!(play.duration, Num::Unknown))
            .count();
        let builders_unknown = scene
            .builders
            .values()
            .filter(|builder| builder.target.is_none())
            .count();
        scenes.push(SceneCoverage {
            name: scene.qualified_name.clone(),
            path: sources.file(scene.file).relative_path().to_owned(),
            constructor_state_unknown: scene.constructor_state_unknown,
            plays: scene.plays.len(),
            plays_with_unknown_duration: plays_unknown,
            builders: scene.builders.len(),
            builders_with_unknown_target: builders_unknown,
            helper_inline_fallbacks: helper_inline_fallbacks(lifecycle, scene),
        });
    }
    scenes.sort_by(|a, b| a.name.cmp(&b.name));

    let project = project_totals(&files, &scenes, lifecycle.inline_fallbacks.len());
    CoverageReport {
        knowledge_profile: profile.name.clone(),
        target_python: target_python.to_owned(),
        files,
        scenes,
        project,
    }
}

fn file_coverage(
    file: &SourceFile,
    target_python: &str,
    profile: &KnowledgeProfile,
    index: &ProjectIndex,
    calls: &QualifiedCallFacts,
) -> FileCoverage {
    let mut coverage = FileCoverage {
        path: file.relative_path().to_owned(),
        parsed: file.is_parsed(),
        gated_constructs: 0,
        unresolved_star_imports: 0,
        unresolved_relative_imports: 0,
        calls: 0,
        unresolved_calls: 0,
        unresolved_call_names: BTreeMap::new(),
        apis_not_in_profile: BTreeSet::new(),
    };

    if let (Some(module), Some(target)) =
        (file.ast(), features::parse_python_version(target_python))
    {
        coverage.gated_constructs = features::violations(file.tokens(), module, target).len();
        if let Some(identity) = index.module_of_file.get(&file.id()) {
            let mut import_stmts = Vec::new();
            collect_import_froms(&module.body, &mut import_stmts);
            for stmt in import_stmts {
                match import_from_names(stmt, identity) {
                    ImportedNames::Star { module, .. } => {
                        let resolved = match module.as_deref() {
                            Some("manim") => true,
                            Some(source) => {
                                !source.starts_with("manim.") && index.module_tree.contains(source)
                            }
                            None => false,
                        };
                        if !resolved {
                            coverage.unresolved_star_imports += 1;
                        }
                    }
                    ImportedNames::Bindings(bindings) => {
                        coverage.unresolved_relative_imports += bindings
                            .iter()
                            .filter(|binding| binding.target == ImportTarget::Unknown)
                            .count();
                    }
                }
            }
        }
    }

    for call in calls.calls_in_file(file.id()) {
        coverage.calls += 1;
        if call.candidates.is_empty() {
            coverage.unresolved_calls += 1;
            let name = call
                .callee_dotted
                .as_ref()
                .map_or_else(|| "<dynamic>".to_owned(), |dotted| dotted.join("."));
            *coverage.unresolved_call_names.entry(name).or_default() += 1;
        }
        for candidate in &call.candidates {
            if candidate.starts_with("manim.") && !profile_knows(profile, candidate) {
                coverage.apis_not_in_profile.insert(candidate.clone());
            }
        }
    }
    coverage
}

/// Whether the knowledge profile describes a canonical `manim.*`
/// candidate: either the id itself is curated, or (for `Class.method`
/// candidates) some curated ancestor along the profile's base chain
/// defines the method — the same resolution the interpreter's dispatch
/// applies.
fn profile_knows(profile: &KnowledgeProfile, candidate: &str) -> bool {
    if profile.symbols.contains_key(candidate) {
        return true;
    }
    let Some((class_id, method)) = candidate.rsplit_once('.') else {
        return false;
    };
    let mut queue = vec![class_id.to_owned()];
    let mut visited = BTreeSet::new();
    while let Some(current) = queue.pop() {
        if !visited.insert(current.clone()) {
            continue;
        }
        if profile.symbols.contains_key(&format!("{current}.{method}")) {
            return true;
        }
        if let Some(entry) = profile.symbol(&current) {
            for base in &entry.bases {
                queue.push(base.clone());
            }
        }
    }
    false
}

/// Collects every `from ... import ...` statement, recursing through
/// nested bodies (imports inside functions, classes, and branches count
/// toward the same namespace honesty).
fn collect_import_froms<'a>(stmts: &'a [ast::Stmt], out: &mut Vec<&'a ast::StmtImportFrom>) {
    for stmt in stmts {
        match stmt {
            ast::Stmt::ImportFrom(import) => out.push(import),
            ast::Stmt::FunctionDef(def) => collect_import_froms(&def.body, out),
            ast::Stmt::AsyncFunctionDef(def) => collect_import_froms(&def.body, out),
            ast::Stmt::ClassDef(def) => collect_import_froms(&def.body, out),
            ast::Stmt::If(inner) => {
                collect_import_froms(&inner.body, out);
                collect_import_froms(&inner.orelse, out);
            }
            ast::Stmt::While(inner) => {
                collect_import_froms(&inner.body, out);
                collect_import_froms(&inner.orelse, out);
            }
            ast::Stmt::For(inner) => {
                collect_import_froms(&inner.body, out);
                collect_import_froms(&inner.orelse, out);
            }
            ast::Stmt::AsyncFor(inner) => {
                collect_import_froms(&inner.body, out);
                collect_import_froms(&inner.orelse, out);
            }
            ast::Stmt::With(inner) => collect_import_froms(&inner.body, out),
            ast::Stmt::AsyncWith(inner) => collect_import_froms(&inner.body, out),
            ast::Stmt::Try(inner) => {
                collect_import_froms(&inner.body, out);
                for handler in &inner.handlers {
                    let ast::ExceptHandler::ExceptHandler(handler) = handler;
                    collect_import_froms(&handler.body, out);
                }
                collect_import_froms(&inner.orelse, out);
                collect_import_froms(&inner.finalbody, out);
            }
            ast::Stmt::TryStar(inner) => {
                collect_import_froms(&inner.body, out);
                for handler in &inner.handlers {
                    let ast::ExceptHandler::ExceptHandler(handler) = handler;
                    collect_import_froms(&handler.body, out);
                }
                collect_import_froms(&inner.orelse, out);
                collect_import_froms(&inner.finalbody, out);
            }
            ast::Stmt::Match(inner) => {
                for case in &inner.cases {
                    collect_import_froms(&case.body, out);
                }
            }
            _ => {}
        }
    }
}

fn project_totals(
    files: &[FileCoverage],
    scenes: &[SceneCoverage],
    inline_fallbacks: usize,
) -> ProjectCoverage {
    let mut totals = ProjectCoverage {
        files: files.len(),
        files_parsed: files.iter().filter(|file| file.parsed).count(),
        gated_constructs: files.iter().map(|file| file.gated_constructs).sum(),
        unresolved_star_imports: files.iter().map(|file| file.unresolved_star_imports).sum(),
        unresolved_relative_imports: files
            .iter()
            .map(|file| file.unresolved_relative_imports)
            .sum(),
        calls: files.iter().map(|file| file.calls).sum(),
        unresolved_calls: files.iter().map(|file| file.unresolved_calls).sum(),
        top_unresolved_call_names: Vec::new(),
        plays: scenes.iter().map(|scene| scene.plays).sum(),
        plays_with_unknown_duration: scenes
            .iter()
            .map(|scene| scene.plays_with_unknown_duration)
            .sum(),
        scenes: scenes.len(),
        scenes_with_unknown_constructor_state: scenes
            .iter()
            .filter(|scene| scene.constructor_state_unknown)
            .count(),
        builders: scenes.iter().map(|scene| scene.builders).sum(),
        builders_with_unknown_target: scenes
            .iter()
            .map(|scene| scene.builders_with_unknown_target)
            .sum(),
        apis_not_in_profile: files
            .iter()
            .flat_map(|file| file.apis_not_in_profile.iter().cloned())
            .collect(),
        // The frontier fact is recorded project-wide (deduplicated
        // across scenes sharing a helper chain), so the total comes
        // straight from the lifecycle fact layer rather than a
        // per-scene sum.
        helper_inline_fallbacks: Some(inline_fallbacks),
    };

    let mut names: BTreeMap<&str, usize> = BTreeMap::new();
    for file in files {
        for (name, count) in &file.unresolved_call_names {
            *names.entry(name).or_default() += count;
        }
    }
    let mut ranked: Vec<NameCount> = names
        .into_iter()
        .map(|(name, count)| NameCount {
            name: name.to_owned(),
            count,
        })
        .collect();
    ranked.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    ranked.truncate(5);
    totals.top_unresolved_call_names = ranked;
    totals
}

/// Renders the grouped human-readable report (per file, per scene, then
/// project totals and the confidence line). Byte-stable for identical
/// inputs.
#[must_use]
pub fn render_text(report: &CoverageReport) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "analysis coverage (knowledge profile {profile}, target-python {target})",
        profile = report.knowledge_profile,
        target = report.target_python,
    );
    for file in report.files.iter().filter(|file| file.has_findings()) {
        render_file_section(&mut out, file);
    }
    for scene in &report.scenes {
        render_scene_section(&mut out, scene);
    }
    render_project_section(&mut out, &report.project);
    render_confidence_line(&mut out, &report.project);
    out
}

/// Renders one flagged file's section (only files with findings appear).
fn render_file_section(out: &mut String, file: &FileCoverage) {
    let _ = writeln!(out);
    let _ = writeln!(out, "{}", file.path);
    if !file.parsed {
        let _ = writeln!(out, "  not parsed (decode or syntax failure; file skipped)");
        return;
    }
    if file.gated_constructs > 0 {
        let _ = writeln!(
            out,
            "  constructs above target-python (MLC000): {}",
            file.gated_constructs
        );
    }
    if file.unresolved_star_imports > 0 {
        let _ = writeln!(
            out,
            "  star imports from unresolved modules: {}",
            file.unresolved_star_imports
        );
    }
    if file.unresolved_relative_imports > 0 {
        let _ = writeln!(
            out,
            "  unresolved relative imports: {}",
            file.unresolved_relative_imports
        );
    }
    if file.unresolved_calls > 0 {
        let names = file
            .unresolved_call_names
            .iter()
            .map(|(name, count)| format!("{name} x{count}"))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            out,
            "  calls with no resolved target: {unresolved} of {total} ({names})",
            unresolved = file.unresolved_calls,
            total = file.calls,
        );
    }
    if !file.apis_not_in_profile.is_empty() {
        let _ = writeln!(
            out,
            "  manim APIs not in the knowledge profile: {}",
            file.apis_not_in_profile
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}

/// Renders one scene's section (every discovered scene appears).
fn render_scene_section(out: &mut String, scene: &SceneCoverage) {
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "scene {name} ({path})",
        name = scene.name,
        path = scene.path
    );
    if scene.constructor_state_unknown {
        let _ = writeln!(
            out,
            "  constructor state unknown (unresolved base chain); lifecycle \
             rules stayed conservative for this scene"
        );
    }
    let _ = writeln!(
        out,
        "  plays with unknown duration: {unknown} of {total}",
        unknown = scene.plays_with_unknown_duration,
        total = scene.plays,
    );
    let _ = writeln!(
        out,
        "  .animate builders with unknown target: {unknown} of {total}",
        unknown = scene.builders_with_unknown_target,
        total = scene.builders,
    );
    if let Some(fallbacks) = scene.helper_inline_fallbacks {
        let _ = writeln!(out, "  helper calls summarized, not inlined: {fallbacks}");
    }
}

/// Renders the project-totals section.
fn render_project_section(out: &mut String, project: &ProjectCoverage) {
    let _ = writeln!(out);
    let _ = writeln!(out, "project");
    let _ = writeln!(
        out,
        "  files parsed: {parsed} of {total}",
        parsed = project.files_parsed,
        total = project.files,
    );
    let _ = writeln!(
        out,
        "  calls resolved: {resolved} of {total}",
        resolved = project.calls - project.unresolved_calls,
        total = project.calls,
    );
    let _ = writeln!(
        out,
        "  play durations known: {known} of {total}",
        known = project.plays - project.plays_with_unknown_duration,
        total = project.plays,
    );
    let _ = writeln!(
        out,
        "  scene constructors resolved: {resolved} of {total}",
        resolved = project.scenes - project.scenes_with_unknown_constructor_state,
        total = project.scenes,
    );
    let _ = writeln!(
        out,
        "  constructs above target-python (MLC000): {}",
        project.gated_constructs
    );
    let _ = writeln!(
        out,
        "  unresolved imports: {total} ({stars} star, {relative} relative)",
        total = project.unresolved_star_imports + project.unresolved_relative_imports,
        stars = project.unresolved_star_imports,
        relative = project.unresolved_relative_imports,
    );
    let _ = writeln!(
        out,
        "  manim APIs not in the knowledge profile: {}",
        project.apis_not_in_profile.len()
    );
    if let Some(fallbacks) = project.helper_inline_fallbacks {
        let _ = writeln!(out, "  helper calls summarized, not inlined: {fallbacks}");
    }
    if !project.top_unresolved_call_names.is_empty() {
        let names = project
            .top_unresolved_call_names
            .iter()
            .map(|entry| format!("{name} x{count}", name = entry.name, count = entry.count))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "  top unresolved calls: {names}");
    }
}

/// Renders the final confidence line: simple `resolved / total` count
/// pairs, explicitly labeled as counts (never invented percentages).
fn render_confidence_line(out: &mut String, project: &ProjectCoverage) {
    let _ = writeln!(out);
    let mut confidence = vec![
        format!("{}/{} files parsed", project.files_parsed, project.files),
        format!(
            "{}/{} calls resolved",
            project.calls - project.unresolved_calls,
            project.calls
        ),
    ];
    if project.plays > 0 {
        confidence.push(format!(
            "{}/{} play durations known",
            project.plays - project.plays_with_unknown_duration,
            project.plays
        ));
    }
    if project.scenes > 0 {
        confidence.push(format!(
            "{}/{} scene constructors resolved",
            project.scenes - project.scenes_with_unknown_constructor_state,
            project.scenes
        ));
    }
    let _ = writeln!(
        out,
        "analysis confidence: {} (counts of analyzed facts, not estimates)",
        confidence.join(", ")
    );
}

/// Renders the machine-readable JSON document, terminated by one newline.
///
/// Top-level keys: `knowledge_profile`, `target_python`, `files`,
/// `scenes`, `project` (see the field docs above for each object's keys;
/// `helper_inline_fallbacks` appears on `project` only — the frontier is
/// recorded project-wide). Keys are stable; numbers are counts of facts.
#[must_use]
pub fn render_json(report: &CoverageReport) -> String {
    let mut output =
        serde_json::to_string_pretty(report).expect("coverage serialization cannot fail");
    output.push('\n');
    output
}
