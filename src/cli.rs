//! Command-line interface (DESIGN §8.1).
//!
//! Exit codes: `0` = no diagnostic meets both `min-confidence` and
//! `fail-level`; `1` = at least one does; `2` = CLI / config / internal
//! error. clap's own usage errors also exit with 2.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use crate::application;
use crate::config::model::Renderer;
use crate::diagnostic::{Confidence, Severity};
use crate::reporting::OutputFormat;
use crate::reporting::coverage::CoverageFormat;

/// Process exit semantics (DESIGN §8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    /// No diagnostic met both `min-confidence` and `fail-level`.
    Success,
    /// At least one diagnostic met both thresholds.
    Failure,
    /// CLI, configuration, or internal error.
    Error,
}

impl ExitStatus {
    /// Numeric exit code.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::Failure => 1,
            Self::Error => 2,
        }
    }
}

impl From<ExitStatus> for ExitCode {
    fn from(status: ExitStatus) -> Self {
        Self::from(status.code())
    }
}

/// `WIDTHxHEIGHT` value for `--resolution`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolution {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl std::str::FromStr for Resolution {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let error =
            || format!("invalid resolution: {value} (expected WIDTHxHEIGHT, e.g. 1920x1080)");
        let (width, height) = value.split_once(['x', 'X']).ok_or_else(error)?;
        Ok(Self {
            width: width.trim().parse().map_err(|_| error())?,
            height: height.trim().parse().map_err(|_| error())?,
        })
    }
}

/// Top-level CLI arguments.
#[derive(Debug, Parser)]
#[command(
    name = "manim-lint",
    version,
    about = "Static lifecycle and performance analysis for Manim scenes"
)]
pub struct Cli {
    /// Subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Subcommands (DESIGN §8.1).
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Analyze Python sources and report diagnostics.
    Check(CheckArgs),
    /// Print the documentation for a rule ID.
    Explain {
        /// Rule ID, e.g. MLC000.
        rule: String,
    },
    /// List every reserved rule ID with phase and implementation status.
    Rules,
    /// Print the resolved effective configuration.
    Config,
    /// Report the symbolic cost breakdown for a scene (Phase 3).
    Cost {
        /// File or directory to analyze.
        path: PathBuf,
        /// Restrict the report to one scene.
        #[arg(long)]
        scene: Option<String>,
    },
    /// Report what the analysis could and could not resolve (unresolved
    /// imports and calls, unknown durations, profile gaps).
    Coverage {
        /// Files or directories to analyze (default: current directory).
        paths: Vec<PathBuf>,
        /// Output format (text|json).
        #[arg(long, default_value = "text")]
        format: CoverageFormat,
    },
}

/// Options for `manim-lint check`.
#[derive(Debug, Args, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool mirrors one independent CLI flag from DESIGN §8.1"
)]
pub struct CheckArgs {
    /// Files or directories to analyze (default: current directory).
    pub paths: Vec<PathBuf>,
    /// Enable only these rule selectors (repeatable).
    #[arg(long)]
    pub select: Vec<String>,
    /// Disable these rule selectors (repeatable).
    #[arg(long)]
    pub ignore: Vec<String>,
    /// Minimum confidence to report (certain|high|medium|low).
    #[arg(long)]
    pub min_confidence: Option<Confidence>,
    /// Severity threshold for exit code 1 (error|warning|info).
    #[arg(long)]
    pub fail_level: Option<Severity>,
    /// Render profile to analyze, or `all`.
    #[arg(long)]
    pub profile: Option<String>,
    /// Override the renderer (cairo|opengl).
    #[arg(long)]
    pub renderer: Option<Renderer>,
    /// Override the frame rate.
    #[arg(long)]
    pub fps: Option<f64>,
    /// Override the resolution as `WIDTHxHEIGHT`.
    #[arg(long)]
    pub resolution: Option<Resolution>,
    /// Output format (concise|full|json|sarif|github).
    #[arg(long, default_value = "concise")]
    pub format: OutputFormat,
    /// Apply safe fixes.
    #[arg(long)]
    pub fix: bool,
    /// Also apply unsafe fixes (requires --fix).
    #[arg(long)]
    pub unsafe_fixes: bool,
    /// Filter out diagnostics recorded in this baseline file.
    #[arg(long)]
    pub baseline: Option<PathBuf>,
    /// Write the current diagnostics to this baseline file.
    #[arg(long)]
    pub write_baseline: Option<PathBuf>,
    /// Disable the analysis cache (no cache exists in Phase 0).
    #[arg(long)]
    pub no_cache: bool,
    /// Print per-rule diagnostic counts to stderr.
    #[arg(long)]
    pub statistics: bool,
    /// Print an analysis-coverage summary to stderr after the diagnostics
    /// (never changes stdout or the exit code).
    #[arg(long)]
    pub analysis_summary: bool,
}

/// Binary entry point: parses arguments, runs, prints, and maps exit codes.
#[must_use]
pub fn run() -> ExitCode {
    let cli = Cli::parse();
    match application::execute(cli.command) {
        Ok(execution) => {
            print!("{}", execution.stdout);
            if let Some(stderr) = &execution.stderr {
                eprint!("{stderr}");
            }
            execution.exit.into()
        }
        Err(error) => {
            eprintln!("manim-lint: error: {error}");
            ExitStatus::Error.into()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_match_design() {
        assert_eq!(ExitStatus::Success.code(), 0);
        assert_eq!(ExitStatus::Failure.code(), 1);
        assert_eq!(ExitStatus::Error.code(), 2);
    }

    #[test]
    fn resolution_parses_width_x_height() {
        let resolution: Resolution = "1920x1080".parse().expect("valid");
        assert_eq!(
            resolution,
            Resolution {
                width: 1920,
                height: 1080
            }
        );
        assert!("1920".parse::<Resolution>().is_err());
        assert!("ax b".parse::<Resolution>().is_err());
    }

    #[test]
    fn cli_parses_check_options() {
        let cli = Cli::try_parse_from([
            "manim-lint",
            "check",
            "scenes",
            "--select",
            "MLC",
            "--min-confidence",
            "medium",
            "--fail-level",
            "error",
            "--resolution",
            "854x480",
            "--format",
            "json",
        ])
        .expect("parses");
        let Command::Check(args) = cli.command else {
            panic!("expected check");
        };
        assert_eq!(args.paths, vec![PathBuf::from("scenes")]);
        assert_eq!(args.select, vec!["MLC".to_owned()]);
        assert_eq!(args.min_confidence, Some(Confidence::Medium));
        assert_eq!(args.fail_level, Some(Severity::Error));
        assert_eq!(
            args.resolution,
            Some(Resolution {
                width: 854,
                height: 480
            })
        );
        assert_eq!(args.format, OutputFormat::Json);
    }

    #[test]
    fn cli_rejects_unknown_format() {
        assert!(Cli::try_parse_from(["manim-lint", "check", "--format", "xml"]).is_err());
    }

    #[test]
    fn cli_parses_the_coverage_subcommand_and_analysis_summary_flag() {
        let cli = Cli::try_parse_from(["manim-lint", "coverage", "scenes", "--format", "json"])
            .expect("parses");
        let Command::Coverage { paths, format } = cli.command else {
            panic!("expected coverage");
        };
        assert_eq!(paths, vec![PathBuf::from("scenes")]);
        assert_eq!(format, CoverageFormat::Json);
        assert!(Cli::try_parse_from(["manim-lint", "coverage", "--format", "xml"]).is_err());

        let cli =
            Cli::try_parse_from(["manim-lint", "check", "--analysis-summary"]).expect("parses");
        let Command::Check(args) = cli.command else {
            panic!("expected check");
        };
        assert!(args.analysis_summary);
    }
}
