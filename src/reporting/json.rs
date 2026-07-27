//! JSON v1 envelope rendering (`schemas/diagnostics-v1.json`).
//!
//! Paths are project-relative POSIX; line/column are one-based Unicode
//! character positions with exclusive ends; unknown optionals serialize as
//! `null` and empty sets as `[]`. Output is byte-stable for identical
//! inputs.

use serde::Serialize;

use crate::diagnostic::Diagnostic;
use crate::reporting::RenderContext;

#[derive(Serialize)]
struct Envelope<'a> {
    schema_version: u32,
    tool_version: &'a str,
    project_root: &'a str,
    profiles: &'a [String],
    diagnostics: &'a [Diagnostic],
}

/// Renders the diagnostics-v1 JSON envelope, terminated by one newline.
#[must_use]
pub fn render(diagnostics: &[Diagnostic], context: &RenderContext<'_>) -> String {
    let envelope = Envelope {
        schema_version: 1,
        tool_version: context.tool_version,
        project_root: context.project_root,
        profiles: context.profiles,
        diagnostics,
    };
    let mut output =
        serde_json::to_string_pretty(&envelope).expect("diagnostic serialization cannot fail");
    output.push('\n');
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::{Confidence, Severity, SourcePosition, SourceSpan};

    #[test]
    fn envelope_always_serializes_schema_required_keys() {
        let diagnostic = Diagnostic {
            rule_id: "MLC000".to_owned(),
            severity: Severity::Error,
            confidence: Confidence::Certain,
            path: "a.py".to_owned(),
            primary_span: SourceSpan {
                start: SourcePosition { line: 1, column: 1 },
                end: SourcePosition { line: 1, column: 1 },
            },
            message: "m".to_owned(),
            explanation: None,
            related_locations: Vec::new(),
            evidence: std::collections::BTreeMap::new(),
            estimated_cost: None,
            applicable_profiles: Vec::new(),
            fix: None,
        };
        let profiles = ["default".to_owned()];
        let context = RenderContext::without_source("0.1.0", ".", &profiles);
        let output = render(&[diagnostic], &context);
        let value: serde_json::Value = serde_json::from_str(&output).expect("valid JSON");

        for key in [
            "schema_version",
            "tool_version",
            "project_root",
            "profiles",
            "diagnostics",
        ] {
            assert!(value.get(key).is_some(), "missing envelope key {key}");
        }
        let diagnostic = &value["diagnostics"][0];
        for key in [
            "rule_id",
            "severity",
            "confidence",
            "path",
            "span",
            "message",
            "applicable_profiles",
            "evidence",
            "fix",
        ] {
            assert!(
                diagnostic.get(key).is_some(),
                "missing diagnostic key {key}"
            );
        }
        assert!(diagnostic["fix"].is_null(), "fix must serialize as null");
        assert!(diagnostic["evidence"].is_object());
        assert_eq!(diagnostic["applicable_profiles"], serde_json::json!([]));
    }
}
