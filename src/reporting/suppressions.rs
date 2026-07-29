//! Inline suppression comments (DESIGN §8.3).
//!
//! - `code  # qual: ignore[MLC108]` suppresses that *statement's*
//!   diagnostics.
//! - A standalone `# qual: ignore[...]` comment applies to the next
//!   statement.
//! - `# qual: file-ignore[MLP]` applies to the whole file and is only
//!   allowed in the header region (shebang / encoding declaration / module
//!   docstring).
//! - An unknown rule ID inside a suppression produces its own `MLC001`
//!   warning and suppresses nothing. Unknown IDs in *config* are exit 2
//!   instead (validated by the config loader).
//!
//! # Statement granularity
//!
//! "Statement" is resolved against the AST at line granularity, recursing
//! into every compound-statement suite:
//!
//! - a *simple* statement covers its whole source extent, continuation
//!   lines included — `self.play(\n ... \n)` is one span, so both the
//!   end-of-line form (a comment on any of its lines) and the standalone
//!   form (a comment on the line above it) suppress diagnostics anchored
//!   anywhere inside the call;
//! - a *compound* statement (`def`, `class`, `if`, `for`, `while`,
//!   `with`, `try`, `match`, and their `async` / `elif` variants) covers
//!   only its **header** lines — from its first line to the line of the
//!   colon opening its suite — never its body. Suppressing a whole suite
//!   with one comment would hide unrelated findings.
//!
//! A standalone comment *inside* a multi-line statement (e.g. within a
//! parenthesized argument list) belongs to that statement, not to the
//! statement after it. A suppression covers every diagnostic whose primary
//! span **starts** within the target statement's line span. When the file
//! has no AST (syntax error), the Phase 0 line behavior is the fallback:
//! end-of-line comments target their own line, standalone comments the
//! next line holding code.

use std::collections::BTreeMap;

use rustpython_parser::Tok;
use rustpython_parser::ast::{self, Ranged};

use crate::diagnostic::Diagnostic;
use crate::rules::registry;
use crate::source::{Comment, SourceFile};

/// Suppressions collected from one file's comments.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SuppressionIndex {
    /// Selectors applying to diagnostics whose primary span starts within
    /// an inclusive one-based line interval (a statement's span, see the
    /// module docs).
    span_selectors: BTreeMap<(usize, usize), Vec<String>>,
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
        let line = diagnostic.primary_span.start.line;
        self.span_selectors
            .iter()
            .any(|(&(start, end), selectors)| {
                (start..=end).contains(&line)
                    && selectors
                        .iter()
                        .any(|selector| selector_matches(selector, &diagnostic.rule_id))
            })
    }

    /// Whether the index contains no suppressions at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.span_selectors.is_empty() && self.file_selectors.is_empty()
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
    let statements = statement_spans(file);
    let code_lines = code_lines(file);
    let header_end = header_end_line(file);

    for comment in file.comments() {
        let Some(directive) = comment.text.find("qual:") else {
            continue;
        };
        let rest = comment.text[directive + "qual:".len()..].trim_start();
        match parse_directive(rest) {
            Some((DirectiveKind::Ignore, selectors)) => {
                let target = target_span(statements.as_deref(), &code_lines, comment);
                let valid = validate(selectors, file, comment, &mut warnings);
                if let Some(span) = target {
                    index.span_selectors.entry(span).or_default().extend(valid);
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
                 `qual: ignore[RULE, ...]` or `qual: file-ignore[RULE, ...]`",
            )),
        }
    }
    (index, warnings)
}

/// The inclusive line interval an `ignore` comment applies to (DESIGN §8.3
/// and the module docs).
///
/// With a parsed AST: a comment on a line some statement covers (an
/// end-of-line comment, or a standalone comment inside a multi-line
/// statement's continuation) targets the innermost covering statement; a
/// standalone comment between statements targets the next statement. An
/// end-of-line comment no statement covers (e.g. on a bare `else:` line)
/// falls back to its own line. Without an AST the Phase 0 line behavior
/// applies throughout.
fn target_span(
    statements: Option<&[(usize, usize)]>,
    code_lines: &[usize],
    comment: &Comment,
) -> Option<(usize, usize)> {
    if let Some(spans) = statements {
        if let Some(covering) = innermost_covering(spans, comment.line) {
            return Some(covering);
        }
        if comment.own_line {
            return next_span(spans, comment.line);
        }
        return Some((comment.line, comment.line));
    }
    if comment.own_line {
        next_code_line(code_lines, comment.line).map(|line| (line, line))
    } else {
        Some((comment.line, comment.line))
    }
}

/// The innermost statement span covering `line`: among covering spans the
/// one starting last (most nested), narrowest on a tie (inline suites).
fn innermost_covering(spans: &[(usize, usize)], line: usize) -> Option<(usize, usize)> {
    spans
        .iter()
        .copied()
        .filter(|(start, end)| (*start..=*end).contains(&line))
        .max_by_key(|&(start, end)| (start, std::cmp::Reverse(end)))
}

/// The first statement span starting after `line` (spans are sorted).
fn next_span(spans: &[(usize, usize)], line: usize) -> Option<(usize, usize)> {
    let position = spans.partition_point(|(start, _)| *start <= line);
    spans.get(position).copied()
}

/// Inclusive one-based line spans of every statement (module docs), or
/// `None` when the file has no AST.
fn statement_spans(file: &SourceFile) -> Option<Vec<(usize, usize)>> {
    let module = file.ast()?;
    let colons = suite_colon_offsets(file);
    let mut spans = Vec::new();
    collect_statement_spans(file, &colons, &module.body, &mut spans);
    spans.sort_unstable();
    spans.dedup();
    Some(spans)
}

/// Byte offsets of every `:` token at bracket depth zero, sorted. Inside a
/// compound statement's range, the first such colon is the one opening its
/// suite (colons in argument lists, subscripts, dict displays, and
/// annotations sit inside brackets; f-strings lex as single tokens).
fn suite_colon_offsets(file: &SourceFile) -> Vec<usize> {
    let mut depth = 0_usize;
    let mut colons = Vec::new();
    for (token, range) in file.tokens() {
        match token {
            Tok::Lpar | Tok::Lsqb | Tok::Lbrace => depth += 1,
            Tok::Rpar | Tok::Rsqb | Tok::Rbrace => depth = depth.saturating_sub(1),
            Tok::Colon if depth == 0 => colons.push(range.start().into()),
            _ => {}
        }
    }
    colons
}

/// Records each statement's span and recurses into compound suites.
fn collect_statement_spans(
    file: &SourceFile,
    colons: &[usize],
    statements: &[ast::Stmt],
    spans: &mut Vec<(usize, usize)>,
) {
    for statement in statements {
        let range = statement.range();
        let start_byte = usize::from(range.start());
        let start = file.line_of_byte(start_byte);
        let suites = child_suites(statement);
        if suites.is_empty() {
            // Simple statement: the whole extent, continuation lines
            // included.
            spans.push((start, file.line_of_byte(range.end().into())));
        } else {
            // Compound statement: header lines only — up to the colon
            // opening its suite, so a comment line between the header and
            // the first body statement stays attributable to that body
            // statement.
            let position = colons.partition_point(|offset| *offset < start_byte);
            let header_end = colons
                .get(position)
                .filter(|offset| **offset < usize::from(range.end()))
                .map_or(start, |offset| file.line_of_byte(*offset).max(start));
            spans.push((start, header_end));
            for suite in suites {
                collect_statement_spans(file, colons, suite, spans);
            }
        }
    }
}

/// The nested statement suites of a compound statement (empty for simple
/// statements).
fn child_suites(statement: &ast::Stmt) -> Vec<&[ast::Stmt]> {
    match statement {
        ast::Stmt::FunctionDef(inner) => vec![&inner.body],
        ast::Stmt::AsyncFunctionDef(inner) => vec![&inner.body],
        ast::Stmt::ClassDef(inner) => vec![&inner.body],
        ast::Stmt::If(inner) => vec![&inner.body, &inner.orelse],
        ast::Stmt::While(inner) => vec![&inner.body, &inner.orelse],
        ast::Stmt::For(inner) => vec![&inner.body, &inner.orelse],
        ast::Stmt::AsyncFor(inner) => vec![&inner.body, &inner.orelse],
        ast::Stmt::With(inner) => vec![&inner.body],
        ast::Stmt::AsyncWith(inner) => vec![&inner.body],
        ast::Stmt::Try(inner) => {
            let mut suites: Vec<&[ast::Stmt]> = vec![&inner.body];
            for handler in &inner.handlers {
                let ast::ExceptHandler::ExceptHandler(handler) = handler;
                suites.push(&handler.body);
            }
            suites.push(&inner.orelse);
            suites.push(&inner.finalbody);
            suites
        }
        ast::Stmt::TryStar(inner) => {
            let mut suites: Vec<&[ast::Stmt]> = vec![&inner.body];
            for handler in &inner.handlers {
                let ast::ExceptHandler::ExceptHandler(handler) = handler;
                suites.push(&handler.body);
            }
            suites.push(&inner.orelse);
            suites.push(&inner.finalbody);
            suites
        }
        ast::Stmt::Match(inner) => inner
            .cases
            .iter()
            .map(|case| case.body.as_slice())
            .collect(),
        _ => Vec::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirectiveKind {
    Ignore,
    FileIgnore,
}

/// Parses `ignore[...]` / `file-ignore[...]` after the `qual:` marker.
fn parse_directive(rest: &str) -> Option<(DirectiveKind, Vec<String>)> {
    let (kind, after) = if let Some(after) = rest.strip_prefix("file-ignore") {
        (DirectiveKind::FileIgnore, after)
    } else {
        let after = rest.strip_prefix("ignore")?;
        (DirectiveKind::Ignore, after)
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
        let sources = file_from("x = 1  # qual: ignore[MLC108]\ny = 2\n");
        let (index, warnings) = collect(&sources.files()[0]);
        assert!(warnings.is_empty());
        assert!(index.suppresses(&diagnostic_at(1, "MLC108")));
        assert!(!index.suppresses(&diagnostic_at(2, "MLC108")));
        assert!(!index.suppresses(&diagnostic_at(1, "MLC101")));
    }

    #[test]
    fn standalone_suppression_applies_to_next_statement() {
        let sources = file_from("# qual: ignore[MLP201]\n\nlabel = 1\nother = 2\n");
        let (index, warnings) = collect(&sources.files()[0]);
        assert!(warnings.is_empty());
        assert!(index.suppresses(&diagnostic_at(3, "MLP201")));
        assert!(!index.suppresses(&diagnostic_at(4, "MLP201")));
    }

    #[test]
    fn standalone_suppression_covers_the_whole_multiline_statement() {
        // The review example: the diagnostic anchors on the `self.play(`
        // line *and* rules may anchor deeper inside the continuation; the
        // standalone form must cover the entire statement.
        let sources = file_from(
            "# qual: ignore[MLC102]\n\
             self.play(\n\
             \x20   square.shift(RIGHT)\n\
             )\n\
             other = 1\n",
        );
        let (index, warnings) = collect(&sources.files()[0]);
        assert!(warnings.is_empty());
        assert!(index.suppresses(&diagnostic_at(2, "MLC102")), "start line");
        assert!(
            index.suppresses(&diagnostic_at(3, "MLC102")),
            "continuation line"
        );
        assert!(index.suppresses(&diagnostic_at(4, "MLC102")), "close line");
        assert!(
            !index.suppresses(&diagnostic_at(5, "MLC102")),
            "the next statement is not covered"
        );
        assert!(!index.suppresses(&diagnostic_at(2, "MLC104")));
    }

    #[test]
    fn end_of_line_suppression_covers_the_whole_multiline_statement() {
        for text in [
            // Comment on the first line of the statement…
            "self.play(  # qual: ignore[MLC102]\n\
             \x20   square.shift(RIGHT)\n\
             )\n",
            // …and on its closing line: both are "the same statement".
            "self.play(\n\
             \x20   square.shift(RIGHT)\n\
             )  # qual: ignore[MLC102]\n",
        ] {
            let sources = file_from(text);
            let (index, warnings) = collect(&sources.files()[0]);
            assert!(warnings.is_empty());
            for line in 1..=3 {
                assert!(
                    index.suppresses(&diagnostic_at(line, "MLC102")),
                    "line {line} of {text:?}"
                );
            }
        }
    }

    #[test]
    fn standalone_comment_inside_a_parenthesized_continuation() {
        // A comment *inside* the statement belongs to that statement, not
        // to the statement after it.
        let sources = file_from(
            "self.play(\n\
             \x20   # qual: ignore[MLC102]\n\
             \x20   square.shift(RIGHT)\n\
             )\n\
             other = 1\n",
        );
        let (index, warnings) = collect(&sources.files()[0]);
        assert!(warnings.is_empty());
        assert!(index.suppresses(&diagnostic_at(1, "MLC102")));
        assert!(index.suppresses(&diagnostic_at(3, "MLC102")));
        assert!(!index.suppresses(&diagnostic_at(5, "MLC102")));
    }

    #[test]
    fn stacked_standalone_comments_all_reach_the_next_statement() {
        let sources = file_from(
            "# qual: ignore[MLC102]\n\
             # qual: ignore[MLC104]\n\
             self.play(\n\
             \x20   square.shift(RIGHT)\n\
             )\n",
        );
        let (index, warnings) = collect(&sources.files()[0]);
        assert!(warnings.is_empty());
        assert!(index.suppresses(&diagnostic_at(3, "MLC102")));
        assert!(index.suppresses(&diagnostic_at(4, "MLC104")));
        assert!(!index.suppresses(&diagnostic_at(3, "MLC101")));
    }

    #[test]
    fn compound_statement_suppression_covers_the_header_not_the_body() {
        let sources = file_from(
            "# qual: ignore[MLP201]\n\
             for item in items:\n\
             \x20   process(item)\n",
        );
        let (index, warnings) = collect(&sources.files()[0]);
        assert!(warnings.is_empty());
        assert!(index.suppresses(&diagnostic_at(2, "MLP201")), "header line");
        assert!(
            !index.suppresses(&diagnostic_at(3, "MLP201")),
            "a suite is never suppressed wholesale"
        );
    }

    #[test]
    fn nested_statements_resolve_to_the_innermost_span() {
        let sources = file_from(
            "def construct(self):\n\
             \x20   self.play(\n\
             \x20       square.shift(RIGHT)\n\
             \x20   )  # qual: ignore[MLC102]\n\
             \x20   self.add(square)\n",
        );
        let (index, warnings) = collect(&sources.files()[0]);
        assert!(warnings.is_empty());
        assert!(index.suppresses(&diagnostic_at(2, "MLC102")));
        assert!(index.suppresses(&diagnostic_at(3, "MLC102")));
        assert!(
            !index.suppresses(&diagnostic_at(5, "MLC102")),
            "the sibling statement is not covered"
        );
        assert!(
            !index.suppresses(&diagnostic_at(1, "MLC102")),
            "the enclosing def header is not covered"
        );
    }

    #[test]
    fn standalone_comment_at_the_top_of_a_body_reaches_the_first_statement() {
        // The comment sits between the `def` header and the first body
        // statement: it belongs to the body statement, not the header.
        let sources = file_from(
            "def construct(self):\n\
             \x20   # qual: ignore[MLC101]\n\
             \x20   self.play(\n\
             \x20   )\n\
             \x20   self.play()\n",
        );
        let (index, warnings) = collect(&sources.files()[0]);
        assert!(warnings.is_empty());
        assert!(index.suppresses(&diagnostic_at(3, "MLC101")));
        assert!(index.suppresses(&diagnostic_at(4, "MLC101")));
        assert!(!index.suppresses(&diagnostic_at(1, "MLC101")), "header");
        assert!(!index.suppresses(&diagnostic_at(5, "MLC101")), "sibling");
    }

    #[test]
    fn multiline_def_header_is_one_span_up_to_its_colon() {
        let sources = file_from(
            "# qual: ignore[MLC105]\n\
             def helper(\n\
             \x20   value,\n\
             ):\n\
             \x20   return value\n",
        );
        let (index, warnings) = collect(&sources.files()[0]);
        assert!(warnings.is_empty());
        for line in 2..=4 {
            assert!(
                index.suppresses(&diagnostic_at(line, "MLC105")),
                "header line {line}"
            );
        }
        assert!(!index.suppresses(&diagnostic_at(5, "MLC105")), "body");
    }

    #[test]
    fn unparsable_file_falls_back_to_line_matching() {
        // `def = 1` lexes but does not parse: no AST, Phase 0 fallback.
        let sources = file_from(
            "x = 1  # qual: ignore[MLC108]\n\
             # qual: ignore[MLP201]\n\
             y = 2\n\
             def = 1\n",
        );
        let file = &sources.files()[0];
        assert!(file.ast().is_none(), "fixture must fail to parse");
        let (index, warnings) = collect(file);
        assert!(warnings.is_empty());
        assert!(index.suppresses(&diagnostic_at(1, "MLC108")));
        assert!(index.suppresses(&diagnostic_at(3, "MLP201")));
        assert!(!index.suppresses(&diagnostic_at(4, "MLP201")));
    }

    #[test]
    fn prefix_selector_suppresses_whole_category() {
        let sources = file_from("x = 1  # qual: ignore[MLP]\n");
        let (index, _) = collect(&sources.files()[0]);
        assert!(index.suppresses(&diagnostic_at(1, "MLP201")));
        assert!(!index.suppresses(&diagnostic_at(1, "MLC101")));
    }

    #[test]
    fn file_ignore_in_header_applies_to_whole_file() {
        let sources =
            file_from("#!/usr/bin/env python\n# qual: file-ignore[MLP]\n\"\"\"doc\"\"\"\nx = 1\n");
        let (index, warnings) = collect(&sources.files()[0]);
        assert!(warnings.is_empty());
        assert!(index.suppresses(&diagnostic_at(4, "MLP201")));
        assert!(!index.suppresses(&diagnostic_at(4, "MLC101")));
    }

    #[test]
    fn file_ignore_after_header_warns_and_does_not_suppress() {
        let sources = file_from("x = 1\n# qual: file-ignore[MLP]\ny = 2\n");
        let (index, warnings) = collect(&sources.files()[0]);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].rule_id, "MLC001");
        assert!(!index.suppresses(&diagnostic_at(3, "MLP201")));
    }

    #[test]
    fn unknown_rule_id_warns_and_does_not_suppress() {
        let sources = file_from("x = 1  # qual: ignore[MLC999, MLC108]\n");
        let (index, warnings) = collect(&sources.files()[0]);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].rule_id, "MLC001");
        assert!(warnings[0].message.contains("MLC999"));
        // The valid ID in the same directive still suppresses.
        assert!(index.suppresses(&diagnostic_at(1, "MLC108")));
        assert!(!index.suppresses(&diagnostic_at(1, "MLC999")));
    }

    #[test]
    fn unknown_rule_id_on_a_multiline_statement_still_warns() {
        // The statement-span rework must not change MLC001 semantics.
        let sources = file_from(
            "# qual: ignore[MLC999]\n\
             self.play(\n\
             \x20   square.shift(RIGHT)\n\
             )\n",
        );
        let (index, warnings) = collect(&sources.files()[0]);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].rule_id, "MLC001");
        assert!(!index.suppresses(&diagnostic_at(2, "MLC999")));
    }

    #[test]
    fn malformed_directive_warns() {
        let sources = file_from("x = 1  # qual: ignore MLC108\n");
        let (index, warnings) = collect(&sources.files()[0]);
        assert_eq!(warnings.len(), 1);
        assert!(index.is_empty());
    }

    #[test]
    fn docstring_end_line_bounds_the_header() {
        let sources = file_from("\"\"\"multi\nline\ndoc\"\"\"\n# qual: file-ignore[MLD]\nx = 1\n");
        let (index, warnings) = collect(&sources.files()[0]);
        // Comment is on line 4, docstring ends on line 3: outside the header.
        assert_eq!(warnings.len(), 1);
        assert!(index.is_empty());
    }
}
