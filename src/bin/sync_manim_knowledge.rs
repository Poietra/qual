//! `sync_manim_knowledge` — knowledge-profile candidate generator and
//! drift check (DESIGN §5.4, §11.2 layer 9).
//!
//! Statically reads a Manim checkout (never importing or executing it),
//! extracts reviewable candidates (classes, base chains, methods,
//! returns-`self` evidence, `__all__` lists, the star-export closure), and
//! optionally diffs them against a curated profile:
//!
//! ```text
//! sync_manim_knowledge --manim-root ../manim --emit candidates.json
//! sync_manim_knowledge --manim-root ../manim --diff            # upstream_0_20
//! sync_manim_knowledge --manim-root ../manim --diff my.json --report drift.json
//! ```
//!
//! Exit codes: `0` no contradictions, `1` the curated profile contradicts
//! the source (category b), `2` usage / I/O / profile errors. Generated
//! output is byte-identical for identical input; humans review and commit
//! curated profiles separately — this tool never edits them.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

use manim_lint::knowledge::generator::{self, DriftReport, GeneratedCandidates};
use manim_lint::knowledge::model::{KnowledgeProfile, ProfileDocument};

/// Command-line arguments.
#[derive(Debug, Parser)]
#[command(
    name = "sync_manim_knowledge",
    about = "Generate knowledge-profile candidates from Manim source and check drift",
    version
)]
struct Cli {
    /// Manim checkout root (containing `manim/__init__.py`) or the package
    /// directory itself. Read-only.
    #[arg(long, value_name = "PATH")]
    manim_root: PathBuf,

    /// Write the generated candidates JSON to this path.
    #[arg(long, value_name = "PATH")]
    emit: Option<PathBuf>,

    /// Diff the candidates against a curated profile: a shipped profile
    /// name or a path to a profile JSON file. Defaults to the shipped
    /// `upstream_0_20` profile when given without a value.
    #[arg(
        long,
        value_name = "PROFILE",
        num_args = 0..=1,
        default_missing_value = manim_lint::knowledge::DEFAULT_PROFILE
    )]
    diff: Option<String>,

    /// Write the machine-readable drift report JSON to this path
    /// (diff mode only).
    #[arg(long, value_name = "PATH")]
    report: Option<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("sync_manim_knowledge: error: {message}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: &Cli) -> Result<ExitCode, String> {
    let candidates = generator::generate(&cli.manim_root).map_err(|error| error.to_string())?;
    if let Some(path) = &cli.emit {
        let json = generator::to_stable_json(&candidates).map_err(|error| error.to_string())?;
        std::fs::write(path, json)
            .map_err(|error| format!("failed to write `{}`: {error}", path.display()))?;
        println!("wrote candidates to {}", path.display());
    }
    let Some(profile_ref) = &cli.diff else {
        print_summary(&candidates);
        return Ok(ExitCode::SUCCESS);
    };
    let profile = load_profile(profile_ref)?;
    let report = generator::diff(&candidates, &profile);
    print!("{}", report.render_text());
    if let Some(path) = &cli.report {
        write_report(path, &report)?;
    }
    if report.has_contradictions() {
        Ok(ExitCode::from(1))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

fn print_summary(candidates: &GeneratedCandidates) {
    println!("generated candidates from package `{}`", candidates.package);
    println!("  source digest: {}", candidates.source_digest);
    println!("  symbols: {}", candidates.symbols.len());
    println!(
        "  star exports: {} resolved, {} module objects, {} unresolved (closure complete: {})",
        candidates.exports.len(),
        candidates.export_modules.len(),
        candidates.unresolved_exports.len(),
        if candidates.exports_complete {
            "yes"
        } else {
            "no"
        }
    );
    println!(
        "  modules: {} with static __all__, {} with dynamic __all__, {} unparsed",
        candidates.module_all.len(),
        candidates.dynamic_all_modules.len(),
        candidates.unparsed_modules.len()
    );
}

/// Loads a curated profile by shipped name, falling back to a JSON file
/// path. Overlay documents must be loaded by shipped name so their base
/// resolves.
fn load_profile(profile_ref: &str) -> Result<KnowledgeProfile, String> {
    match manim_lint::knowledge::load(profile_ref) {
        Ok(profile) => Ok(profile),
        Err(load_error) => {
            let path = PathBuf::from(profile_ref);
            if !path.is_file() {
                return Err(load_error.to_string());
            }
            let json = std::fs::read_to_string(&path)
                .map_err(|error| format!("failed to read `{}`: {error}", path.display()))?;
            let document = ProfileDocument::from_json(profile_ref, &json)
                .map_err(|error| error.to_string())?;
            document.into_resolved().map_err(|error| error.to_string())
        }
    }
}

fn write_report(path: &PathBuf, report: &DriftReport) -> Result<(), String> {
    let json = generator::to_stable_json(report).map_err(|error| error.to_string())?;
    std::fs::write(path, json)
        .map_err(|error| format!("failed to write `{}`: {error}", path.display()))?;
    println!("wrote drift report to {}", path.display());
    Ok(())
}
