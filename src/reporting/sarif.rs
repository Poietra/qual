//! SARIF 2.1.0 rendering (DESIGN §8.1, Phase 5 pulled forward).
//!
//! Generated with `serde_json` only — no external SARIF dependency. In this
//! build `serde_json::Map` is `BTreeMap`-backed (the `preserve_order`
//! feature is off), so every object serializes with sorted keys and the
//! output is byte-deterministic for identical inputs.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::diagnostic::{Diagnostic, Severity};
use crate::reporting::RenderContext;
use crate::rules::registry;

/// URL advertised as the tool driver's `informationUri`.
const INFORMATION_URI: &str = "https://github.com/example/manim-lint";

/// Canonical SARIF 2.1.0 schema location referenced from the log.
const SARIF_SCHEMA: &str =
    "https://docs.oasis-open.org/sarif/sarif/v2.1.0/errata01/os/schemas/sarif-schema-2.1.0.json";

/// Renders diagnostics as one SARIF 2.1.0 run, terminated by one newline.
///
/// Conventions match the JSON envelope (DESIGN §6.3): artifact URIs are
/// project-relative POSIX paths, and regions use one-based Unicode
/// character columns with exclusive ends, declared on the run as
/// `columnKind: "unicodeCodePoints"`. Severities map `error → error`,
/// `warning → warning`, and `info → note`.
#[must_use]
pub fn render(diagnostics: &[Diagnostic], context: &RenderContext<'_>) -> String {
    let results: Vec<Value> = diagnostics.iter().map(result).collect();
    let log = json!({
        "$schema": SARIF_SCHEMA,
        "version": "2.1.0",
        "runs": [{
            "columnKind": "unicodeCodePoints",
            "tool": {
                "driver": {
                    "name": "manim-lint",
                    "version": context.tool_version,
                    "informationUri": INFORMATION_URI,
                    "rules": rule_descriptors(diagnostics),
                },
            },
            "results": results,
        }],
    });
    let mut output = serde_json::to_string_pretty(&log).expect("SARIF serialization cannot fail");
    output.push('\n');
    output
}

/// One `reportingDescriptor` per distinct rule ID appearing in the results,
/// sorted by rule ID for deterministic output.
fn rule_descriptors(diagnostics: &[Diagnostic]) -> Vec<Value> {
    let rule_ids: BTreeSet<&str> = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.rule_id.as_str())
        .collect();
    rule_ids
        .into_iter()
        .map(|rule_id| {
            let summary =
                registry::metadata_for(rule_id).map_or(rule_id, |metadata| metadata.summary);
            json!({
                "id": rule_id,
                "shortDescription": { "text": summary },
            })
        })
        .collect()
}

fn result(diagnostic: &Diagnostic) -> Value {
    json!({
        "ruleId": diagnostic.rule_id,
        "level": level(diagnostic.severity),
        "message": { "text": diagnostic.message },
        "locations": [{
            "physicalLocation": {
                "artifactLocation": { "uri": diagnostic.path },
                "region": {
                    "startLine": diagnostic.primary_span.start.line,
                    "startColumn": diagnostic.primary_span.start.column,
                    "endLine": diagnostic.primary_span.end.line,
                    "endColumn": diagnostic.primary_span.end.column,
                },
            },
        }],
    })
}

/// SARIF `level` for a diagnostic severity.
const fn level(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "note",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::{Confidence, SourcePosition, SourceSpan};
    use std::collections::BTreeMap;

    fn diagnostic(rule_id: &str, severity: Severity) -> Diagnostic {
        Diagnostic {
            rule_id: rule_id.to_owned(),
            severity,
            confidence: Confidence::Certain,
            path: "scenes/demo.py".to_owned(),
            primary_span: SourceSpan {
                start: SourcePosition { line: 3, column: 5 },
                end: SourcePosition { line: 3, column: 9 },
            },
            message: "message".to_owned(),
            explanation: None,
            related_locations: Vec::new(),
            evidence: BTreeMap::new(),
            estimated_cost: None,
            applicable_profiles: Vec::new(),
            fix: None,
        }
    }

    #[test]
    fn severity_levels_map_to_sarif_levels() {
        let context = RenderContext {
            tool_version: "0.1.0",
            project_root: ".",
            profiles: &[],
        };
        let output = render(
            &[
                diagnostic("MLC000", Severity::Error),
                diagnostic("MLC001", Severity::Warning),
                diagnostic("MLP209", Severity::Info),
            ],
            &context,
        );
        let value: Value = serde_json::from_str(&output).expect("valid JSON");
        let results = value["runs"][0]["results"].as_array().expect("results");
        assert_eq!(results[0]["level"], "error");
        assert_eq!(results[1]["level"], "warning");
        assert_eq!(results[2]["level"], "note");
    }

    #[test]
    fn every_reported_rule_gets_a_descriptor_with_a_short_description() {
        let context = RenderContext {
            tool_version: "0.1.0",
            project_root: ".",
            profiles: &[],
        };
        let output = render(
            &[
                diagnostic("MLC001", Severity::Warning),
                diagnostic("MLC000", Severity::Error),
                diagnostic("MLC000", Severity::Error),
            ],
            &context,
        );
        let value: Value = serde_json::from_str(&output).expect("valid JSON");
        let rules = value["runs"][0]["tool"]["driver"]["rules"]
            .as_array()
            .expect("rules");
        // Distinct rules, sorted by ID.
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0]["id"], "MLC000");
        assert_eq!(rules[1]["id"], "MLC001");
        for rule in rules {
            assert!(rule["shortDescription"]["text"].is_string());
        }
    }
}
