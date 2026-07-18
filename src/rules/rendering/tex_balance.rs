//! `MLR110`: literal TeX brace / environment imbalance, conservative
//! parser (DESIGN §7.2 + binding prose).
//!
//! The verdict mirrors what the sibling Manim checkout actually compiles
//! (`tex_mobject.py`):
//!
//! - `SingleStringMathTex._remove_stray_braces` *repairs* count
//!   imbalances (prepends `{` / appends `}`), so a pure count mismatch is
//!   NOT an error. What survives the repair is a structural error: the
//!   running group depth going negative mid-string (`a}b{c`), or a
//!   `\begin{...}` / `\end{...}` mismatch.
//! - `MathTex` / `Tex` compile all positional parts joined with
//!   `arg_separator` (plus balanced `\special{...}` wrappers that cannot
//!   change depth), so parts are scanned jointly, never individually.
//! - `\left` / `\right` imbalance is repaired (`→ \big`), an unmatched
//!   `array` environment blanks the whole string, and Manim's double-brace
//!   notation (`{{ ... }}`) splits the input — all of those are silence,
//!   not errors.
//!
//! The prose binding (DESIGN §7.2): TeX macros, comments, verbatim,
//! conditionals, catcode games, and custom environments make brace
//! counting unreliable — any such construct yields `Unknown` and silence.

use std::collections::BTreeMap;

use rustpython_parser::text_size::TextRange;
use serde_json::json;

use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::frontend::index::LiteralFact;
use crate::rules::base::{Rule, RuleContext};

use super::{build_diagnostic, short_name, single_knowledge_symbol};

/// Constructors whose joined positional literals compile as one TeX
/// expression, with (default `arg_separator`, default `tex_environment`).
const TEX_CONSTRUCTORS: &[(&str, &str, &str)] = &[
    ("manim.mobject.text.tex_mobject.MathTex", " ", "align*"),
    (
        "manim.mobject.text.tex_mobject.SingleStringMathTex",
        " ",
        "align*",
    ),
    ("manim.mobject.text.tex_mobject.Tex", "", "center"),
];

/// Commands whose presence makes brace counting unreliable (macros,
/// verbatim, grouping primitives, catcode games, file input). Command
/// names starting with `if`, plus `else` / `fi` / `or`, are handled
/// separately (skipped-conditional branches do not execute braces).
const BLOCKLIST_COMMANDS: &[&str] = &[
    "begingroup",
    "bgroup",
    "catcode",
    "csname",
    "def",
    "edef",
    "egroup",
    "endcsname",
    "endgroup",
    "endinput",
    "gdef",
    "include",
    "input",
    "let",
    "makeatletter",
    "makeatother",
    "newcommand",
    "newenvironment",
    "providecommand",
    "renewcommand",
    "renewenvironment",
    "verb",
    "xdef",
];

/// Delimiter-sizing commands whose *next* token is a delimiter argument:
/// a bare `{` / `}` after one of these is a delimiter (or a TeX error),
/// never a group — Unknown.
const DELIMITER_COMMANDS: &[&str] = &[
    "Big", "Bigg", "Biggl", "Biggm", "Biggr", "Bigl", "Bigm", "Bigr", "big", "bigg", "biggl",
    "biggm", "biggr", "bigl", "bigm", "bigr", "left", "middle", "right",
];

const MLR110: RuleMetadata = RuleMetadata {
    id: "MLR110",
    summary: "Literal TeX has a definite brace/environment imbalance",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::High,
    implementation_phase: 4,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

pub(super) struct UnbalancedTexLiteral;

impl Rule for UnbalancedTexLiteral {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR110
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let profiles = context.config().active_profile_names();
        let mut diagnostics = Vec::new();
        for call in &context.qualified_calls().calls {
            let Some((constructor_id, _)) = single_knowledge_symbol(profile, &call.candidates)
            else {
                continue;
            };
            let Some((_, default_separator, default_environment)) = TEX_CONSTRUCTORS
                .iter()
                .find(|(id, _, _)| *id == constructor_id)
            else {
                continue;
            };
            // Splats hide parts; the isolate machinery rewrites the joined
            // string: both are Unknown.
            if call.has_star_args
                || call.has_star_star_kwargs
                || call.keyword("substrings_to_isolate").is_some()
                || call.keyword("tex_to_color_map").is_some()
            {
                continue;
            }
            let Some(separator) = effective_separator(call, default_separator) else {
                continue;
            };
            let Some(wrapper) = effective_environment(call, default_environment) else {
                continue;
            };
            // Every positional part must be a static string (numbers
            // stringify digits-only, which cannot unbalance anything).
            let mut parts: Vec<(String, TextRange)> = Vec::new();
            let mut all_literal = true;
            for position in 0..call.positional_count {
                let Some(argument) = call.positional(position) else {
                    all_literal = false;
                    break;
                };
                match &argument.literal {
                    Some(LiteralFact::Str {
                        value,
                        prefix,
                        range,
                    }) if !prefix.bytes => {
                        parts.push((value.clone(), *range));
                    }
                    Some(LiteralFact::Int(value)) => {
                        parts.push((value.to_string(), argument.range));
                    }
                    Some(LiteralFact::Float(value)) => {
                        parts.push((value.to_string(), argument.range));
                    }
                    _ => {
                        all_literal = false;
                        break;
                    }
                }
            }
            if !all_literal || parts.is_empty() {
                continue;
            }
            let Some(error) = joined_tex_error(&parts, &separator, wrapper.as_deref()) else {
                continue;
            };
            let file = context.sources().file(call.file);
            let constructor = short_name(constructor_id);
            let mut evidence = BTreeMap::new();
            evidence.insert("constructor".to_owned(), json!(constructor_id));
            evidence.insert("error".to_owned(), json!(error.label()));
            if let Some(environment) = &error.environment() {
                evidence.insert("environment".to_owned(), json!(environment));
            }
            diagnostics.push(build_diagnostic(
                &MLR110,
                file,
                error.range(),
                Confidence::High,
                format!(
                    "`{constructor}()` literal has a definite TeX error: {}",
                    error.message()
                ),
                "Manim repairs pure brace-count imbalances (stray braces are \
                 re-balanced before compiling), but a closing brace that arrives \
                 before its group opens, or a mismatched \\begin/\\end pair, \
                 survives the repair and fails the TeX compile at render time. \
                 Only the conservative literal subset is judged: macros, comments, \
                 verbatim, conditionals, and custom environments are left alone.",
                evidence,
                profiles.clone(),
                None,
            ));
        }
        diagnostics
    }
}

/// The literal `arg_separator` in effect, `None` when unknowable or when
/// it could itself change brace analysis.
fn effective_separator(
    call: &crate::frontend::index::QualifiedCall,
    default_separator: &str,
) -> Option<String> {
    let separator = match call.keyword("arg_separator") {
        None => default_separator.to_owned(),
        Some(argument) => match &argument.literal {
            Some(LiteralFact::Str { value, prefix, .. }) if !prefix.bytes => value.clone(),
            _ => return None,
        },
    };
    if separator
        .chars()
        .any(|character| matches!(character, '{' | '}' | '\\' | '%'))
    {
        return None;
    }
    Some(separator)
}

/// The effective wrapper environment name: `Some(None)` for an explicit
/// `tex_environment=None`, `None` (skip) when unknowable.
#[allow(
    clippy::option_option,
    reason = "outer None = unknowable, inner None = no wrapper"
)]
fn effective_environment(
    call: &crate::frontend::index::QualifiedCall,
    default_environment: &str,
) -> Option<Option<String>> {
    match call.keyword("tex_environment") {
        None => Some(Some(default_environment.to_owned())),
        Some(argument) => match &argument.literal {
            Some(LiteralFact::Str { value, prefix, .. }) if !prefix.bytes => {
                Some(Some(value.clone()))
            }
            Some(LiteralFact::NoneLit) => Some(None),
            _ => None,
        },
    }
}

/// A definite TeX error located in one literal part.
#[derive(Debug, PartialEq, Eq)]
enum TexError {
    /// A `}` closed a group that was never opened (post-repair).
    NegativeDepth { range: TextRange },
    /// `\end{found}` closed `\begin{open}`.
    CrossedEnvironments {
        range: TextRange,
        open: String,
        found: String,
    },
    /// `\end{name}` without any open environment.
    StrayEnd { range: TextRange, name: String },
    /// `\begin{name}` never closed.
    UnmatchedBegin { range: TextRange, name: String },
}

impl TexError {
    fn range(&self) -> TextRange {
        match self {
            Self::NegativeDepth { range }
            | Self::CrossedEnvironments { range, .. }
            | Self::StrayEnd { range, .. }
            | Self::UnmatchedBegin { range, .. } => *range,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::NegativeDepth { .. } => "negative-brace-depth",
            Self::CrossedEnvironments { .. } => "crossed-environments",
            Self::StrayEnd { .. } => "stray-end",
            Self::UnmatchedBegin { .. } => "unmatched-begin",
        }
    }

    fn environment(&self) -> Option<String> {
        match self {
            Self::NegativeDepth { .. } => None,
            Self::CrossedEnvironments { open, .. } => Some(open.clone()),
            Self::StrayEnd { name, .. } | Self::UnmatchedBegin { name, .. } => Some(name.clone()),
        }
    }

    fn message(&self) -> String {
        match self {
            Self::NegativeDepth { .. } => "a `}` closes a group that is never opened \
                                           before it — TeX stops with \"Too many }'s\""
                .to_owned(),
            Self::CrossedEnvironments { open, found, .. } => format!(
                "`\\begin{{{open}}}` is closed by `\\end{{{found}}}` — TeX stops with \
                 an environment mismatch"
            ),
            Self::StrayEnd { name, .. } => {
                format!("`\\end{{{name}}}` has no matching `\\begin{{{name}}}`")
            }
            Self::UnmatchedBegin { name, .. } => {
                format!("`\\begin{{{name}}}` is never closed by `\\end{{{name}}}`")
            }
        }
    }
}

/// One token of the conservative TeX scan.
enum Token {
    Open,
    Close,
    Begin(String),
    End(String),
    Other,
    /// A construct outside the analyzable subset.
    Unknown,
}

/// Scanner over one string, yielding tokens and exact brace counts.
struct Scanner<'a> {
    text: &'a [u8],
    cursor: usize,
}

impl<'a> Scanner<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text: text.as_bytes(),
            cursor: 0,
        }
    }

    fn command_name(&mut self) -> String {
        let start = self.cursor;
        while self.cursor < self.text.len() && self.text[self.cursor].is_ascii_alphabetic() {
            self.cursor += 1;
        }
        String::from_utf8_lossy(&self.text[start..self.cursor]).into_owned()
    }

    /// Reads `{name}` immediately after `\begin` / `\end`.
    fn environment_name(&mut self) -> Option<String> {
        if self.text.get(self.cursor) != Some(&b'{') {
            return None;
        }
        let start = self.cursor + 1;
        let mut end = start;
        while let Some(byte) = self.text.get(end) {
            if byte.is_ascii_alphanumeric() || *byte == b'*' {
                end += 1;
            } else {
                break;
            }
        }
        if end == start || self.text.get(end) != Some(&b'}') {
            return None;
        }
        self.cursor = end + 1;
        Some(String::from_utf8_lossy(&self.text[start..end]).into_owned())
    }

    fn next_token(&mut self) -> Option<Token> {
        let byte = *self.text.get(self.cursor)?;
        self.cursor += 1;
        match byte {
            b'{' => {
                // Manim's own `{{ ... }}` split notation is out of scope.
                if self.text.get(self.cursor) == Some(&b'{') {
                    return Some(Token::Unknown);
                }
                Some(Token::Open)
            }
            b'}' => {
                if self.text.get(self.cursor) == Some(&b'}') {
                    return Some(Token::Unknown);
                }
                Some(Token::Close)
            }
            b'%' => Some(Token::Unknown),
            b'^' if self.text.get(self.cursor) == Some(&b'^') => Some(Token::Unknown),
            // A control character in the *decoded* value means a Python
            // escape corrupted the intended TeX (`"\begin..."` turns into
            // a backspace): MLR103's territory, Unknown here.
            byte if byte < 0x20 && !matches!(byte, b'\n' | b'\r' | b'\t') => Some(Token::Unknown),
            0x7f => Some(Token::Unknown),
            b'\\' => {
                let Some(next) = self.text.get(self.cursor).copied() else {
                    // A trailing backslash swallows what follows (the
                    // separator / next part): out of scope.
                    return Some(Token::Unknown);
                };
                if !next.is_ascii_alphabetic() {
                    // Control symbol (`\{`, `\}`, `\\`, `\%`, `\,`, ...):
                    // consumed as a unit, no group effect.
                    self.cursor += 1;
                    return Some(Token::Other);
                }
                let name = self.command_name();
                if name == "begin" || name == "end" {
                    let Some(environment) = self.environment_name() else {
                        return Some(Token::Unknown);
                    };
                    return Some(if name == "begin" {
                        Token::Begin(environment)
                    } else {
                        Token::End(environment)
                    });
                }
                if BLOCKLIST_COMMANDS.contains(&name.as_str())
                    || name.starts_with("if")
                    || matches!(name.as_str(), "else" | "fi" | "or")
                {
                    return Some(Token::Unknown);
                }
                if DELIMITER_COMMANDS.contains(&name.as_str())
                    && matches!(self.text.get(self.cursor), Some(&(b'{' | b'}')))
                {
                    // The following brace is a delimiter argument, not a
                    // group (and `\left{` is itself a TeX error).
                    return Some(Token::Unknown);
                }
                Some(Token::Other)
            }
            _ => Some(Token::Other),
        }
    }
}

/// Non-overlapping substring occurrence count (Python `str.count`).
fn substring_count(text: &str, pattern: &str) -> usize {
    if pattern.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut from = 0;
    while let Some(found) = text[from..].find(pattern) {
        count += 1;
        from += found + pattern.len();
    }
    count
}

/// Manim's stray-brace counting formula
/// (`SingleStringMathTex._remove_stray_braces`).
fn manim_brace_counts(text: &str) -> (usize, usize) {
    let lefts =
        substring_count(text, "{") - substring_count(text, "\\{") + substring_count(text, "\\\\{");
    let rights =
        substring_count(text, "}") - substring_count(text, "\\}") + substring_count(text, "\\\\}");
    (lefts, rights)
}

/// Scans the joined literal parts for a definite TeX error. `None` means
/// balanced or Unknown (silence either way).
fn joined_tex_error(
    parts: &[(String, TextRange)],
    separator: &str,
    wrapper: Option<&str>,
) -> Option<TexError> {
    // Pass 1: tokenize every part, enforcing the analyzable subset and
    // collecting exact brace counts.
    let mut exact_lefts = 0usize;
    let mut exact_rights = 0usize;
    let mut tokens: Vec<(usize, Token)> = Vec::new();
    let mut environments_seen: Vec<String> = Vec::new();
    for (position, (text, _)) in parts.iter().enumerate() {
        let mut scanner = Scanner::new(text);
        while let Some(token) = scanner.next_token() {
            match &token {
                Token::Unknown => return None,
                Token::Open => exact_lefts += 1,
                Token::Close => exact_rights += 1,
                Token::Begin(name) | Token::End(name) => {
                    // `\begin{x}` carries one balanced brace pair.
                    exact_lefts += 1;
                    exact_rights += 1;
                    environments_seen.push(name.clone());
                }
                Token::Other => {}
            }
            tokens.push((position, token));
        }
    }
    // The scan and Manim's count-based repair must agree on the joined
    // string, or the repair model is wrong: Unknown.
    let joined: String = {
        let texts: Vec<&str> = parts.iter().map(|(text, _)| text.as_str()).collect();
        texts.join(separator)
    };
    let (manim_lefts, manim_rights) = manim_brace_counts(&joined);
    if (manim_lefts, manim_rights) != (exact_lefts, exact_rights) {
        return None;
    }
    // Manim blanks the whole string when exactly one of \begin{array} /
    // \end{array} appears; a literal touching the wrapper environment can
    // legally split it: both are out of scope.
    let begins_array = substring_count(&joined, "\\begin{array}") > 0;
    let ends_array = substring_count(&joined, "\\end{array}") > 0;
    if begins_array != ends_array {
        return None;
    }
    if let Some(wrapper) = wrapper {
        if environments_seen.iter().any(|name| name == wrapper) {
            return None;
        }
    }
    // Pass 2: depth scan with Manim's repair applied (prepended opens),
    // plus the environment stack.
    let mut depth = i64::try_from(manim_rights.saturating_sub(manim_lefts)).unwrap_or(i64::MAX);
    let mut stack: Vec<(String, i64, usize)> = Vec::new();
    for (position, token) in &tokens {
        match token {
            Token::Open => depth += 1,
            Token::Close => {
                depth -= 1;
                if depth < 0 {
                    return Some(TexError::NegativeDepth {
                        range: parts[*position].1,
                    });
                }
            }
            Token::Begin(name) => stack.push((name.clone(), depth, *position)),
            Token::End(name) => match stack.pop() {
                None => {
                    return Some(TexError::StrayEnd {
                        range: parts[*position].1,
                        name: name.clone(),
                    });
                }
                Some((open, open_depth, open_position)) => {
                    if open != *name {
                        return Some(TexError::CrossedEnvironments {
                            range: parts[*position].1,
                            open,
                            found: name.clone(),
                        });
                    }
                    if open_depth != depth {
                        // Environment crossing a brace group: TeX's exact
                        // behavior depends on the environment — Unknown.
                        let _ = open_position;
                        return None;
                    }
                }
            },
            Token::Other | Token::Unknown => {}
        }
    }
    if let Some((name, _, position)) = stack.into_iter().next() {
        return Some(TexError::UnmatchedBegin {
            range: parts[position].1,
            name,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustpython_parser::text_size::TextSize;

    fn part(text: &str) -> Vec<(String, TextRange)> {
        vec![(
            text.to_owned(),
            TextRange::new(TextSize::from(0), TextSize::from(1)),
        )]
    }

    fn error_label(text: &str) -> Option<&'static str> {
        joined_tex_error(&part(text), " ", Some("align*")).map(|error| error.label())
    }

    #[test]
    fn count_imbalances_are_repaired_by_manim_and_stay_silent() {
        assert_eq!(error_label(r"\frac{a}{b"), None);
        assert_eq!(error_label(r"x}"), None);
        assert_eq!(error_label(r"{x"), None);
    }

    #[test]
    fn negative_depth_survives_the_repair_and_fires() {
        assert_eq!(error_label(r"a}b{c"), Some("negative-brace-depth"));
        assert_eq!(error_label(r"a} b} {c {d"), Some("negative-brace-depth"));
    }

    #[test]
    fn escaped_braces_and_left_right_do_not_count() {
        assert_eq!(error_label(r"\{a\}"), None);
        assert_eq!(error_label(r"\left(\frac{a}{b}\right)"), None);
        // Escaped closer between real braces: still balanced.
        assert_eq!(error_label(r"{a\}b}"), None);
    }

    #[test]
    fn environment_mismatches_fire() {
        assert_eq!(error_label(r"\begin{cases} a"), Some("unmatched-begin"),);
        assert_eq!(error_label(r"a \end{cases}"), Some("stray-end"));
        assert_eq!(
            error_label(r"\begin{cases} a \end{matrix}"),
            Some("crossed-environments"),
        );
        assert_eq!(error_label(r"\begin{cases} a \end{cases}"), None);
    }

    #[test]
    fn out_of_scope_constructs_are_unknown_and_silent() {
        assert_eq!(error_label(r"50\% } off"), None); // \% ok but } is... repaired? no: prepends
        assert_eq!(error_label(r"a % comment }"), None);
        assert_eq!(error_label(r"\def\x{ }"), None);
        assert_eq!(error_label(r"\verb|}|"), None);
        assert_eq!(error_label(r"{{ x }}"), None);
        assert_eq!(error_label(r"\ifx a } \fi"), None);
        assert_eq!(error_label(r"\left{ x"), None);
        assert_eq!(error_label(r"\begin {cases} x"), None);
    }

    #[test]
    fn wrapper_environment_mentions_are_unknown() {
        assert_eq!(error_label(r"\end{align*} x \begin{align*}"), None);
        assert_eq!(
            joined_tex_error(&part(r"a \end{center}"), "", Some("center")),
            None
        );
    }

    #[test]
    fn array_blanking_is_silent() {
        assert_eq!(error_label(r"\begin{array} x"), None);
    }

    #[test]
    fn parts_are_judged_jointly_not_individually() {
        let parts = vec![
            (
                r"e^{i".to_owned(),
                TextRange::new(TextSize::from(0), TextSize::from(4)),
            ),
            (
                r"\tau} = 1".to_owned(),
                TextRange::new(TextSize::from(6), TextSize::from(15)),
            ),
        ];
        assert!(joined_tex_error(&parts, " ", Some("align*")).is_none());
        let crossing = vec![
            (
                r"} closes".to_owned(),
                TextRange::new(TextSize::from(0), TextSize::from(8)),
            ),
            (
                r"{ opens".to_owned(),
                TextRange::new(TextSize::from(10), TextSize::from(17)),
            ),
        ];
        let error = joined_tex_error(&crossing, " ", Some("align*")).expect("depth error");
        assert_eq!(error.label(), "negative-brace-depth");
        // Anchored at the part with the offending close.
        assert_eq!(error.range().start(), TextSize::from(0));
    }
}
