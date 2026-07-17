//! Inline suppression comments (DESIGN §8.3).
//!
//! - `code  # manim-lint: ignore[MLC108]` suppresses that statement's
//!   diagnostics (matched by line in Phase 0).
//! - A standalone `# manim-lint: ignore[...]` comment applies to the next
//!   statement.
//! - `# manim-lint: file-ignore[MLP]` applies to the whole file and is only
//!   allowed in the header region (shebang / encoding declaration / module
//!   docstring).
//! - An unknown rule ID inside a suppression produces its own `MLC001`
//!   warning and suppresses nothing. Unknown IDs in *config* are exit 2
//!   instead (validated by the config loader).

use std::collections::BTreeMap;

use rustpython_parser::Tok;
use rustpython_parser::ast::{self, Ranged};

use crate::diagnostic::Diagnostic;
use crate::rules::registry;
use crate::source::{Comment, SourceFile};

/// Suppressions collected from one file's comments.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SuppressionIndex {
    /// Selectors applying to diagnostics that start on a given line.
    line_selectors: BTreeMap<usize, Vec<String>>,
    /// Selectors applying to every diagnostic in the file.
    file_selectors: Vec<String>,
}

impl SuppressionIndex {
    /// Whether this index suppresses the given diagnostic.
    #[must_use]
    pub fn suppresses(&self, diagnostic: &Diagnostic) -> bool {
        if self
            .file_selectors
            .iter()
            .any(|selector| selector_matches(selector, &diagnostic.rule_id))
        {
            return true;
        }
        self.line_selectors
            .get(&diagnostic.primary_span.start.line)
            .is_some_and(|selectors| {
                selectors
                    .iter()
                    .any(|selector| selector_matches(selector, &diagnostic.rule_id))
            })
    }

    /// Whether the index contains no suppressions at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.line_selectors.is_empty() && self.file_selectors.is_empty()
    }
}

/// Whether a suppression selector (rule ID or prefix) matches a rule ID.
#[must_use]
pub fn selector_matches(selector: &str, rule_id: &str) -> bool {
    selector == rule_id
        || (registry::RULE_PREFIXES.contains(&selector) && rule_id.starts_with(selector))
}

/// Parses every suppression comment in a file.
///
/// Returns the collected index plus `MLC001` diagnostics for malformed
/// directives, unknown rule IDs, and `file-ignore` outside the header.
#[must_use]
pub fn collect(file: &SourceFile) -> (SuppressionIndex, Vec<Diagnostic>) {
    let mut index = SuppressionIndex::default();
    let mut warnings = Vec::new();
    let code_lines = code_lines(file);
    let header_end = header_end_line(file);

    for comment in file.comments() {
        let Some(directive) = comment.text.find("manim-lint:") else {
            continue;
        };
        let rest = comment.text[directive + "manim-lint:".len()..].trim_start();
        match parse_directive(rest) {
            Some((DirectiveKind::Ignore, selectors)) => {
                let target_line = if comment.own_line {
                    next_code_line(&code_lines, comment.line)
                } else {
                    Some(comment.line)
                };
                let valid = validate(selectors, file, comment, &mut warnings);
                if let Some(line) = target_line {
                    index.line_selectors.entry(line).or_default().extend(valid);
                }
            }
            Some((DirectiveKind::FileIgnore, selectors)) => {
                if comment.line > header_end {
                    warnings.push(warning(
                        file,
                        comment,
                        "file-ignore is only allowed in the file header \
                         (shebang, encoding declaration, module docstring)",
                    ));
                    continue;
                }
                let valid = validate(selectors, file, comment, &mut warnings);
                index.file_selectors.extend(valid);
            }
            None => warnings.push(warning(
                file,
                comment,
                "malformed suppression comment; expected \
                 `manim-lint: ignore[RULE, ...]` or `manim-lint: file-ignore[RULE, ...]`",
            )),
        }
    }
    (index, warnings)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirectiveKind {
    Ignore,
    FileIgnore,
}

/// Parses `ignore[...]` / `file-ignore[...]` after the `manim-lint:` marker.
fn parse_directive(rest: &str) -> Option<(DirectiveKind, Vec<String>)> {
    let (kind, after) = if let Some(after) = rest.strip_prefix("file-ignore") {
        (DirectiveKind::FileIgnore, after)
    } else if let Some(after) = rest.strip_prefix("ignore") {
        (DirectiveKind::Ignore, after)
    } else {
        return None;
    };
    let after = after.trim_start();
    let inner = after.strip_prefix('[')?;
    let end = inner.find(']')?;
    let selectors: Vec<String> = inner[..end]
        .split(',')
        .map(str::trim)
        .filter(|selector| !selector.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    if selectors.is_empty() {
        return None;
    }
    Some((kind, selectors))
}

/// Keeps known selectors; each unknown one becomes an `MLC001` warning.
fn validate(
    selectors: Vec<String>,
    file: &SourceFile,
    comment: &Comment,
    warnings: &mut Vec<Diagnostic>,
) -> Vec<String> {
    let mut valid = Vec::new();
    for selector in selectors {
        if registry::RULE_PREFIXES.contains(&selector.as_str())
            || registry::is_reserved_rule_id(&selector)
        {
            valid.push(selector);
        } else {
            warnings.push(warning(
                file,
                comment,
                &format!("unknown rule ID in suppression: {selector}"),
            ));
        }
    }
    valid
}

fn warning(file: &SourceFile, comment: &Comment, message: &str) -> Diagnostic {
    let metadata = &registry::INVALID_SUPPRESSION;
    Diagnostic {
        rule_id: metadata.id.to_owned(),
        severity: metadata.default_severity,
        confidence: metadata.minimum_confidence,
        path: file.relative_path().to_owned(),
        primary_span: file.span_of_range(comment.range),
        message: message.to_owned(),
        explanation: Some("Invalid suppression comments never suppress diagnostics.".to_owned()),
        related_locations: Vec::new(),
        evidence: BTreeMap::new(),
        estimated_cost: None,
        applicable_profiles: Vec::new(),
        fix: None,
    }
}

/// Sorted one-based lines that contain at least one non-trivia token.
fn code_lines(file: &SourceFile) -> Vec<usize> {
    let mut lines: Vec<usize> = file
        .tokens()
        .iter()
        .filter(|(token, _)| {
            !matches!(
                token,
                Tok::Comment(_)
                    | Tok::Newline
                    | Tok::NonLogicalNewline
                    | Tok::Indent
                    | Tok::Dedent
                    | Tok::EndOfFile
            )
        })
        .map(|(_, range)| file.line_of_byte(range.start().into()))
        .collect();
    lines.sort_unstable();
    lines.dedup();
    lines
}

fn next_code_line(code_lines: &[usize], after: usize) -> Option<usize> {
    let position = code_lines.partition_point(|line| *line <= after);
    code_lines.get(position).copied()
}

/// Last one-based line of the header region (shebang / encoding declaration /
/// module docstring). `file-ignore` is allowed on lines `<=` this.
fn header_end_line(file: &SourceFile) -> usize {
    let Some(module) = file.ast() else {
        // Unparsable files only carry MLC000; be permissive.
        return usize::MAX;
    };
    let Some(first) = module.body.first() else {
        return usize::MAX;
    };
    if is_docstring(first) {
        return file.span_of_range(first.range()).end.line;
    }
    file.span_of_range(first.range())
        .start
        .line
        .saturating_sub(1)
}

fn is_docstring(stmt: &ast::Stmt) -> bool {
    let ast::Stmt::Expr(expression) = stmt else {
        return false;
    };
    matches!(
        expression.value.as_ref(),
        ast::Expr::Constant(constant)
            if matches!(constant.value, ast::Constant::Str(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::{Confidence, Severity, SourcePosition, SourceSpan};
    use crate::source::SourceManager;
    use std::path::Path;

    fn file_from(text: &str) -> SourceManager {
        let mut sources = SourceManager::new("/project");
        sources.load_bytes(Path::new("/project/scene.py"), text.as_bytes());
        sources
    }

    fn diagnostic_at(line: usize, rule_id: &str) -> Diagnostic {
        Diagnostic {
            rule_id: rule_id.to_owned(),
            severity: Severity::Warning,
            confidence: Confidence::High,
            path: "scene.py".to_owned(),
            primary_span: SourceSpan {
                start: SourcePosition { line, column: 1 },
                end: SourcePosition { line, column: 2 },
            },
            message: "m".to_owned(),
            explanation: None,
            related_locations: Vec::new(),
            evidence: BTreeMap::new(),
            estimated_cost: None,
            applicable_profiles: Vec::new(),
            fix: None,
        }
    }

    #[test]
    fn end_of_line_suppression_matches_same_line() {
        let sources = file_from("x = 1  # manim-lint: ignore[MLC108]\ny = 2\n");
        let (index, warnings) = collect(&sources.files()[0]);
        assert!(warnings.is_empty());
        assert!(index.suppresses(&diagnostic_at(1, "MLC108")));
        assert!(!index.suppresses(&diagnostic_at(2, "MLC108")));
        assert!(!index.suppresses(&diagnostic_at(1, "MLC101")));
    }

    #[test]
    fn standalone_suppression_applies_to_next_statement() {
        let sources = file_from("# manim-lint: ignore[MLP201]\n\nlabel = 1\nother = 2\n");
        let (index, warnings) = collect(&sources.files()[0]);
        assert!(warnings.is_empty());
        assert!(index.suppresses(&diagnostic_at(3, "MLP201")));
        assert!(!index.suppresses(&diagnostic_at(4, "MLP201")));
    }

    #[test]
    fn prefix_selector_suppresses_whole_category() {
        let sources = file_from("x = 1  # manim-lint: ignore[MLP]\n");
        let (index, _) = collect(&sources.files()[0]);
        assert!(index.suppresses(&diagnostic_at(1, "MLP201")));
        assert!(!index.suppresses(&diagnostic_at(1, "MLC101")));
    }

    #[test]
    fn file_ignore_in_header_applies_to_whole_file() {
        let sources = file_from(
            "#!/usr/bin/env python\n# manim-lint: file-ignore[MLP]\n\"\"\"doc\"\"\"\nx = 1\n",
        );
        let (index, warnings) = collect(&sources.files()[0]);
        assert!(warnings.is_empty());
        assert!(index.suppresses(&diagnostic_at(4, "MLP201")));
        assert!(!index.suppresses(&diagnostic_at(4, "MLC101")));
    }

    #[test]
    fn file_ignore_after_header_warns_and_does_not_suppress() {
        let sources = file_from("x = 1\n# manim-lint: file-ignore[MLP]\ny = 2\n");
        let (index, warnings) = collect(&sources.files()[0]);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].rule_id, "MLC001");
        assert!(!index.suppresses(&diagnostic_at(3, "MLP201")));
    }

    #[test]
    fn unknown_rule_id_warns_and_does_not_suppress() {
        let sources = file_from("x = 1  # manim-lint: ignore[MLC999, MLC108]\n");
        let (index, warnings) = collect(&sources.files()[0]);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].rule_id, "MLC001");
        assert!(warnings[0].message.contains("MLC999"));
        // The valid ID in the same directive still suppresses.
        assert!(index.suppresses(&diagnostic_at(1, "MLC108")));
        assert!(!index.suppresses(&diagnostic_at(1, "MLC999")));
    }

    #[test]
    fn malformed_directive_warns() {
        let sources = file_from("x = 1  # manim-lint: ignore MLC108\n");
        let (index, warnings) = collect(&sources.files()[0]);
        assert_eq!(warnings.len(), 1);
        assert!(index.is_empty());
    }

    #[test]
    fn docstring_end_line_bounds_the_header() {
        let sources =
            file_from("\"\"\"multi\nline\ndoc\"\"\"\n# manim-lint: file-ignore[MLD]\nx = 1\n");
        let (index, warnings) = collect(&sources.files()[0]);
        // Comment is on line 4, docstring ends on line 3: outside the header.
        assert_eq!(warnings.len(), 1);
        assert!(index.is_empty());
    }
}
