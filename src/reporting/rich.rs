//! Rich terminal renderer: severity banner, source frame, and summary.
//!
//! This is the format a person reads. It shows the offending source line with
//! the span underlined, because a line/column pair alone makes the reader open
//! an editor to learn what the finding is talking about.
//!
//! Source text is read lazily from disk rather than taken from the analyzer:
//! a cache hit answers without ever building a `SourceManager`, and the frame
//! must not be the reason that fast path is given up. A file that changed,
//! disappeared, or is too large since the analysis simply renders without its
//! frame — the header, message, and explanation are always shown.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::diagnostic::{Diagnostic, Severity};
use crate::source_limits::MAX_SOURCE_BYTES_V1;

/// Lines of context shown above and below the offending line.
const CONTEXT_LINES: usize = 2;

/// Columns used when the terminal width is unknown.
const DEFAULT_WIDTH: usize = 80;

/// Widest rule line, so a maximized terminal does not draw a banner across
/// the whole screen.
const MAX_WIDTH: usize = 100;

/// Spaces a tab expands to inside a rendered frame.
const TAB_WIDTH: usize = 4;

/// Whether ANSI styling is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorChoice {
    /// Emit ANSI styling.
    Always,
    /// Emit plain text.
    Never,
}

impl ColorChoice {
    const fn style(self, code: &'static str) -> &'static str {
        match self {
            Self::Always => code,
            Self::Never => "",
        }
    }
}

const RESET: &str = "\u{1b}[0m";
const BOLD: &str = "\u{1b}[1m";
const DIM: &str = "\u{1b}[2m";
const RED: &str = "\u{1b}[31m";
const YELLOW: &str = "\u{1b}[33m";
const BLUE: &str = "\u{1b}[34m";
const GREEN: &str = "\u{1b}[32m";

/// Renders diagnostics as banners with source frames, then a summary.
///
/// `project_root` resolves the relative path each diagnostic carries.
#[must_use]
pub fn render(
    diagnostics: &[Diagnostic],
    project_root: &Path,
    color: ColorChoice,
    files_analyzed: usize,
) -> String {
    let width = terminal_width();
    let mut sources = SourceCache::new(project_root);
    let mut output = String::new();

    for diagnostic in diagnostics {
        push_diagnostic(&mut output, diagnostic, &mut sources, color, width);
    }
    push_summary(&mut output, diagnostics, color, files_analyzed);
    output
}

fn push_diagnostic(
    output: &mut String,
    diagnostic: &Diagnostic,
    sources: &mut SourceCache,
    color: ColorChoice,
    width: usize,
) {
    let (icon, accent) = match diagnostic.severity {
        Severity::Error => ("✖", RED),
        Severity::Warning => ("⚠", YELLOW),
        Severity::Info => ("ℹ", BLUE),
    };
    let location = format!(
        "{}:{}:{}",
        diagnostic.path, diagnostic.primary_span.start.line, diagnostic.primary_span.start.column
    );

    // Banner: icon, rule, location, then a rule filling the remaining width.
    let head = format!("{icon} {} {location} ", diagnostic.rule_id);
    let _ = writeln!(
        output,
        "{accent_on}{bold}{icon} {rule}{reset} {dim}{location}{reset} {dim}{rule_line}{reset}",
        accent_on = color.style(accent),
        bold = color.style(BOLD),
        reset = color.style(RESET),
        dim = color.style(DIM),
        rule = diagnostic.rule_id,
        rule_line = "─".repeat(width.saturating_sub(head.chars().count() + 1).max(3)),
    );
    let _ = writeln!(output);

    for line in wrap(&diagnostic.message, width.saturating_sub(4)) {
        let _ = writeln!(output, "  {line}");
    }
    let _ = writeln!(output);

    if let Some(frame) = sources.frame(diagnostic) {
        push_frame(output, &frame, color, accent);
        let _ = writeln!(output);
    }

    if let Some(explanation) = &diagnostic.explanation {
        let mut first = true;
        for line in wrap(explanation, width.saturating_sub(6)) {
            let marker = if first { "ℹ" } else { " " };
            let _ = writeln!(
                output,
                "  {dim}{marker} {line}{reset}",
                dim = color.style(DIM),
                reset = color.style(RESET),
            );
            first = false;
        }
        let _ = writeln!(output);
    }
}

/// One resolved source frame: numbered lines plus the span to underline.
struct Frame {
    lines: Vec<(usize, String)>,
    marked_line: usize,
    underline_start: usize,
    underline_length: usize,
}

fn push_frame(output: &mut String, frame: &Frame, color: ColorChoice, accent: &'static str) {
    let gutter = frame
        .lines
        .iter()
        .map(|(number, _)| decimal_width(*number))
        .max()
        .unwrap_or(1);

    for (number, text) in &frame.lines {
        let marked = *number == frame.marked_line;
        // The arrow is styled only when it is drawn, so an unmarked line does
        // not carry a reset sequence that resets nothing.
        let arrow = if marked {
            format!("{}>{}", color.style(accent), color.style(RESET))
        } else {
            " ".to_owned()
        };
        let _ = writeln!(
            output,
            "  {arrow} {dim}{number:>gutter$} │{reset} {text}",
            reset = color.style(RESET),
            dim = color.style(DIM),
        );
        if marked {
            let _ = writeln!(
                output,
                "    {dim}{blank:>gutter$} │{reset} {padding}{accent_on}{carets}{reset}",
                dim = color.style(DIM),
                reset = color.style(RESET),
                blank = "",
                padding = " ".repeat(frame.underline_start),
                accent_on = color.style(accent),
                carets = "^".repeat(frame.underline_length),
            );
        }
    }
}

fn push_summary(
    output: &mut String,
    diagnostics: &[Diagnostic],
    color: ColorChoice,
    files_analyzed: usize,
) {
    let errors = count(diagnostics, Severity::Error);
    let warnings = count(diagnostics, Severity::Warning);
    let infos = count(diagnostics, Severity::Info);
    let files = format!(
        "{files_analyzed} {}",
        plural(files_analyzed, "file", "files")
    );

    if diagnostics.is_empty() {
        let _ = writeln!(
            output,
            "{green}✔{reset} {bold}No findings{reset} {dim}in {files}{reset}",
            green = color.style(GREEN),
            reset = color.style(RESET),
            bold = color.style(BOLD),
            dim = color.style(DIM),
        );
        return;
    }

    let mut parts = Vec::new();
    if errors > 0 {
        parts.push(format!(
            "{}✖ {errors} {}{}",
            color.style(RED),
            plural(errors, "error", "errors"),
            color.style(RESET)
        ));
    }
    if warnings > 0 {
        parts.push(format!(
            "{}⚠ {warnings} {}{}",
            color.style(YELLOW),
            plural(warnings, "warning", "warnings"),
            color.style(RESET)
        ));
    }
    if infos > 0 {
        parts.push(format!(
            "{}ℹ {infos} {}{}",
            color.style(BLUE),
            plural(infos, "note", "notes"),
            color.style(RESET)
        ));
    }
    let _ = writeln!(
        output,
        "{}  {dim}in {files}{reset}",
        parts.join("  "),
        dim = color.style(DIM),
        reset = color.style(RESET),
    );

    // Only rules that are actually present, so the hint never names a rule
    // the reader cannot look up in this output.
    let mut rules: Vec<&str> = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.rule_id.as_str())
        .collect();
    rules.sort_unstable();
    rules.dedup();
    if let Some(first) = rules.first() {
        let _ = writeln!(
            output,
            "{dim}Run `manim-lint explain {first}` for the full rule documentation.{reset}",
            dim = color.style(DIM),
            reset = color.style(RESET),
        );
    }
}

fn count(diagnostics: &[Diagnostic], severity: Severity) -> usize {
    diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == severity)
        .count()
}

const fn plural(count: usize, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 { one } else { many }
}

fn decimal_width(mut value: usize) -> usize {
    let mut width = 1;
    while value >= 10 {
        value /= 10;
        width += 1;
    }
    width
}

/// Reads and caches the source of each file that carries a diagnostic.
struct SourceCache<'a> {
    project_root: &'a Path,
    files: BTreeMap<String, Option<Vec<String>>>,
}

impl<'a> SourceCache<'a> {
    fn new(project_root: &'a Path) -> Self {
        Self {
            project_root,
            files: BTreeMap::new(),
        }
    }

    fn frame(&mut self, diagnostic: &Diagnostic) -> Option<Frame> {
        let lines = self.lines(&diagnostic.path)?;
        let span = &diagnostic.primary_span;
        let marked_line = span.start.line;
        let text = lines.get(marked_line.checked_sub(1)?)?;

        let first = marked_line.saturating_sub(CONTEXT_LINES).max(1);
        let last = (marked_line + CONTEXT_LINES).min(lines.len());
        let window = (first..=last)
            .filter_map(|number| {
                lines
                    .get(number - 1)
                    .map(|line| (number, expand_tabs(line)))
            })
            .collect::<Vec<_>>();

        // Columns are one-based character columns; a span that ends on a later
        // line is underlined only to the end of the marked line.
        let start = span.start.column.saturating_sub(1);
        let end = if span.end.line == marked_line {
            span.end.column.saturating_sub(1)
        } else {
            text.chars().count()
        };
        let underline_start = display_width(text, start);
        let underline_length = display_width(text, end.max(start)).saturating_sub(underline_start);

        Some(Frame {
            lines: window,
            marked_line,
            underline_start,
            underline_length: underline_length.max(1),
        })
    }

    fn lines(&mut self, relative_path: &str) -> Option<&Vec<String>> {
        if !self.files.contains_key(relative_path) {
            let loaded = read_source_lines(&self.project_root.join(relative_path));
            self.files.insert(relative_path.to_owned(), loaded);
        }
        self.files.get(relative_path)?.as_ref()
    }
}

/// Reads a file for display only, under the same size limit the analyzer
/// applies. Any failure yields `None` and the frame is skipped.
fn read_source_lines(path: &PathBuf) -> Option<Vec<String>> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_SOURCE_BYTES_V1 {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    Some(text.lines().map(ToOwned::to_owned).collect())
}

fn expand_tabs(line: &str) -> String {
    let mut expanded = String::with_capacity(line.len());
    for character in line.chars() {
        if character == '\t' {
            let pad = TAB_WIDTH - (expanded.chars().count() % TAB_WIDTH);
            expanded.extend(std::iter::repeat_n(' ', pad));
        } else {
            expanded.push(character);
        }
    }
    expanded
}

/// Display columns occupied by the first `chars` characters of `line`, after
/// the same tab expansion the frame applies.
fn display_width(line: &str, chars: usize) -> usize {
    let mut width = 0;
    for character in line.chars().take(chars) {
        if character == '\t' {
            width += TAB_WIDTH - (width % TAB_WIDTH);
        } else {
            width += 1;
        }
    }
    width
}

/// Wraps at word boundaries, preserving the paragraph breaks the source text
/// already has. A word longer than the limit is left intact rather than cut.
fn wrap(text: &str, limit: usize) -> Vec<String> {
    let limit = limit.max(20);
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            if !current.is_empty() && current.chars().count() + 1 + word.chars().count() > limit {
                lines.push(std::mem::take(&mut current));
            }
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
        }
        lines.push(current);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

/// Terminal width from `COLUMNS`, clamped so banners stay readable.
fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|width| *width >= 40)
        .unwrap_or(DEFAULT_WIDTH)
        .min(MAX_WIDTH)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::diagnostic::{Confidence, SourcePosition, SourceSpan};

    fn diagnostic(path: &str, line: usize, start: usize, end: usize) -> Diagnostic {
        Diagnostic {
            rule_id: "MLC104".to_owned(),
            severity: Severity::Error,
            confidence: Confidence::Certain,
            path: path.to_owned(),
            primary_span: SourceSpan {
                start: SourcePosition {
                    line,
                    column: start,
                },
                end: SourcePosition { line, column: end },
            },
            message: "Use a positive `run_time`.".to_owned(),
            explanation: Some("Manim validates durations when a play executes.".to_owned()),
            related_locations: Vec::new(),
            evidence: BTreeMap::new(),
            estimated_cost: None,
            applicable_profiles: Vec::new(),
            fix: None,
        }
    }

    fn fixture() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("temp dir");
        let source = "from manim import *\n\n\nclass Demo(Scene):\n    def construct(self):\n        self.play(FadeIn(Square()), run_time=0)\n";
        std::fs::write(dir.path().join("demo.py"), source).expect("write fixture");
        (dir, "demo.py".to_owned())
    }

    #[test]
    fn frame_underlines_the_span_and_marks_its_line() {
        let (dir, path) = fixture();
        let rendered = render(
            &[diagnostic(&path, 6, 37, 47)],
            dir.path(),
            ColorChoice::Never,
            1,
        );
        assert!(rendered.contains("> 6 │         self.play(FadeIn(Square()), run_time=0)"));
        // The caret row aligns under the span, offset by the same gutter.
        let caret_line = rendered
            .lines()
            .find(|line| line.contains('^'))
            .expect("caret row");
        let carets = caret_line.find('^').expect("caret column");
        let source_line = rendered
            .lines()
            .find(|line| line.contains("> 6 │"))
            .expect("marked row");
        let target = source_line.find("run_time").expect("span column");
        assert_eq!(carets, target);
    }

    #[test]
    fn missing_source_still_renders_the_finding() {
        let dir = tempfile::tempdir().expect("temp dir");
        let rendered = render(
            &[diagnostic("gone.py", 3, 1, 5)],
            dir.path(),
            ColorChoice::Never,
            1,
        );
        assert!(rendered.contains("MLC104"));
        assert!(rendered.contains("gone.py:3:1"));
        assert!(!rendered.contains('^'), "no frame without source");
    }

    #[test]
    fn a_span_past_the_end_of_a_changed_file_is_skipped() {
        let (dir, path) = fixture();
        let rendered = render(
            &[diagnostic(&path, 999, 1, 5)],
            dir.path(),
            ColorChoice::Never,
            1,
        );
        assert!(rendered.contains("MLC104"));
        assert!(!rendered.contains('^'));
    }

    #[test]
    fn summary_counts_each_severity_and_pluralizes() {
        let (dir, path) = fixture();
        let mut warning = diagnostic(&path, 6, 37, 47);
        warning.severity = Severity::Warning;
        warning.rule_id = "MLC109".to_owned();
        let rendered = render(
            &[diagnostic(&path, 6, 36, 38), warning],
            dir.path(),
            ColorChoice::Never,
            3,
        );
        assert!(rendered.contains("✖ 1 error"));
        assert!(rendered.contains("⚠ 1 warning"));
        assert!(rendered.contains("in 3 files"));
    }

    #[test]
    fn a_clean_run_says_so() {
        let dir = tempfile::tempdir().expect("temp dir");
        let rendered = render(&[], dir.path(), ColorChoice::Never, 12);
        assert!(rendered.contains("No findings"));
        assert!(rendered.contains("in 12 files"));
    }

    #[test]
    fn color_never_emits_no_escape_sequences() {
        let (dir, path) = fixture();
        let rendered = render(
            &[diagnostic(&path, 6, 37, 47)],
            dir.path(),
            ColorChoice::Never,
            1,
        );
        assert!(!rendered.contains('\u{1b}'));
    }

    #[test]
    fn color_always_emits_escape_sequences() {
        let (dir, path) = fixture();
        let rendered = render(
            &[diagnostic(&path, 6, 37, 47)],
            dir.path(),
            ColorChoice::Always,
            1,
        );
        assert!(rendered.contains('\u{1b}'));
    }

    #[test]
    fn tabs_expand_consistently_in_the_line_and_the_caret_row() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("tabs.py"), "x = 1\n\tvalue = 2\n").expect("write");
        // `value` starts at character column 2 (after the tab).
        let rendered = render(
            &[diagnostic("tabs.py", 2, 2, 7)],
            dir.path(),
            ColorChoice::Never,
            1,
        );
        let source_line = rendered
            .lines()
            .find(|line| line.contains("> 2 │"))
            .expect("marked row");
        let caret_line = rendered
            .lines()
            .find(|line| line.contains('^'))
            .expect("caret row");
        assert_eq!(
            caret_line.find('^').expect("caret"),
            source_line.find("value").expect("value column")
        );
    }
}
