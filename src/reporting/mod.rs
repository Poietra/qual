//! Diagnostic output: text/JSON/SARIF renderers, inline suppressions,
//! baseline files, and fix application.

pub mod baseline;
pub mod coverage;
pub mod fixes;
pub mod json;
pub mod sarif;
pub mod suppressions;
pub mod text;

use crate::diagnostic::Diagnostic;

/// Output format selected with `--format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// One line per diagnostic.
    #[default]
    Concise,
    /// Concise plus explanation, evidence, and applicable profiles.
    Full,
    /// JSON envelope matching `schemas/diagnostics-v1.json`.
    Json,
    /// SARIF 2.1.0.
    Sarif,
    /// GitHub Actions workflow annotations.
    Github,
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Concise => "concise",
            Self::Full => "full",
            Self::Json => "json",
            Self::Sarif => "sarif",
            Self::Github => "github",
        })
    }
}

impl std::str::FromStr for OutputFormat {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "concise" => Ok(Self::Concise),
            "full" => Ok(Self::Full),
            "json" => Ok(Self::Json),
            "sarif" => Ok(Self::Sarif),
            "github" => Ok(Self::Github),
            _ => Err(format!(
                "unknown format: {value} (expected concise|full|json|sarif|github)"
            )),
        }
    }
}

/// Everything the renderers need beyond the diagnostics themselves.
#[derive(Debug, Clone)]
pub struct RenderContext<'a> {
    /// `tool_version` in the JSON envelope.
    pub tool_version: &'a str,
    /// `project_root` in the JSON envelope (POSIX, usually `.`).
    pub project_root: &'a str,
    /// Names of the analyzed profiles.
    pub profiles: &'a [String],
}

/// Renders sorted diagnostics in the chosen format.
///
/// Output is deterministic and byte-stable for identical inputs.
#[must_use]
pub fn render(
    format: OutputFormat,
    diagnostics: &[Diagnostic],
    context: &RenderContext<'_>,
) -> String {
    match format {
        OutputFormat::Concise => text::render_concise(diagnostics),
        OutputFormat::Full => text::render_full(diagnostics),
        OutputFormat::Json => json::render(diagnostics, context),
        OutputFormat::Github => text::render_github(diagnostics),
        OutputFormat::Sarif => sarif::render(diagnostics, context),
    }
}
