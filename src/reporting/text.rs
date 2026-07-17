//! Text renderers: `concise`, `full`, and GitHub Actions annotations.

use std::fmt::Write as _;

use crate::diagnostic::{Diagnostic, Severity};

/// One line per diagnostic: `path:line:col: RULE severity message`.
#[must_use]
pub fn render_concise(diagnostics: &[Diagnostic]) -> String {
    let mut output = String::new();
    for diagnostic in diagnostics {
        push_concise_line(&mut output, diagnostic);
    }
    output
}

/// Concise line plus explanation, evidence, and applicable profiles.
#[must_use]
pub fn render_full(diagnostics: &[Diagnostic]) -> String {
    let mut output = String::new();
    for diagnostic in diagnostics {
        push_concise_line(&mut output, diagnostic);
        if let Some(explanation) = &diagnostic.explanation {
            for line in explanation.lines() {
                let _ = writeln!(output, "    {line}");
            }
        }
        for (key, value) in &diagnostic.evidence {
            let _ = writeln!(output, "    evidence.{key}: {value}");
        }
        if !diagnostic.applicable_profiles.is_empty() {
            let _ = writeln!(
                output,
                "    applies to profiles: {}",
                diagnostic.applicable_profiles.join(", ")
            );
        }
    }
    output
}

/// GitHub Actions `::error` / `::warning` / `::notice` annotations.
#[must_use]
pub fn render_github(diagnostics: &[Diagnostic]) -> String {
    let mut output = String::new();
    for diagnostic in diagnostics {
        let command = match diagnostic.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "notice",
        };
        let _ = writeln!(
            output,
            "::{command} file={file},line={line},col={col},endLine={end_line},endColumn={end_col},title={rule}::{message}",
            file = escape_github_property(&diagnostic.path),
            line = diagnostic.primary_span.start.line,
            col = diagnostic.primary_span.start.column,
            end_line = diagnostic.primary_span.end.line,
            end_col = diagnostic.primary_span.end.column,
            rule = escape_github_property(&diagnostic.rule_id),
            message = escape_github_data(&diagnostic.message),
        );
    }
    output
}

fn push_concise_line(output: &mut String, diagnostic: &Diagnostic) {
    let _ = writeln!(
        output,
        "{path}:{line}:{col}: {rule} {severity} {message}",
        path = diagnostic.path,
        line = diagnostic.primary_span.start.line,
        col = diagnostic.primary_span.start.column,
        rule = diagnostic.rule_id,
        severity = diagnostic.severity,
        message = diagnostic.message,
    );
}

fn escape_github_data(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

fn escape_github_property(value: &str) -> String {
    escape_github_data(value)
        .replace(',', "%2C")
        .replace(':', "%3A")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::{Confidence, SourcePosition, SourceSpan};

    fn sample() -> Diagnostic {
        Diagnostic {
            rule_id: "MLC000".to_owned(),
            severity: Severity::Error,
            confidence: Confidence::Certain,
            path: "scenes/demo.py".to_owned(),
            primary_span: SourceSpan {
                start: SourcePosition { line: 3, column: 5 },
                end: SourcePosition { line: 3, column: 6 },
            },
            message: "syntax error: invalid syntax".to_owned(),
            explanation: Some("The file is skipped.".to_owned()),
            related_locations: Vec::new(),
            evidence: std::collections::BTreeMap::new(),
            estimated_cost: None,
            applicable_profiles: vec!["production".to_owned()],
            fix: None,
        }
    }

    #[test]
    fn concise_is_one_line_per_diagnostic() {
        let output = render_concise(&[sample()]);
        assert_eq!(
            output,
            "scenes/demo.py:3:5: MLC000 error syntax error: invalid syntax\n"
        );
    }

    #[test]
    fn full_includes_explanation_and_profiles() {
        let output = render_full(&[sample()]);
        assert!(output.contains("    The file is skipped.\n"));
        assert!(output.contains("    applies to profiles: production\n"));
    }

    #[test]
    fn github_annotations_escape_newlines() {
        let mut diagnostic = sample();
        diagnostic.message = "line1\nline2 100%".to_owned();
        let output = render_github(&[diagnostic]);
        assert!(output.starts_with("::error file=scenes/demo.py,line=3,col=5,"));
        assert!(output.contains("line1%0Aline2 100%25"));
    }
}
