//! Pre-parse admission limits for untrusted sources (DESIGN §5.2).
//!
//! The analyzer never executes the code it reads, but it does hand it to a
//! recursive-descent parser and to recursive analysis passes. A source whose
//! nesting exceeds the native stack therefore aborts the whole process rather
//! than failing one file, which is the difference between "one project skips a
//! file" and "the service that ran the analyzer died".
//!
//! These limits are checked before parsing so a hostile or generated file is
//! rejected as an ordinary `MLC000` diagnostic. They are deliberately far above
//! anything hand-written Python reaches: the Manim Community fork's own sources
//! peak at bracket depth 15 and 13 indentation levels.

use rustpython_parser::Tok;
use rustpython_parser::text_size::TextRange;

/// Largest source accepted for analysis, in bytes.
pub const MAX_SOURCE_BYTES_V1: usize = 4 * 1024 * 1024;

/// Largest accepted bracket or indentation nesting depth.
pub const MAX_NESTING_DEPTH_V1: u32 = 96;

/// Longest accepted run of consecutive prefix operators (`-`, `+`, `~`, `not`).
///
/// Each prefix operator becomes one more recursive parser frame, so a long run
/// overflows the stack even though the file is small and shallow by every other
/// measure.
pub const MAX_PREFIX_OPERATOR_RUN_V1: u32 = 64;

/// A limit that a source exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceLimitViolation {
    /// The encoded source is larger than [`MAX_SOURCE_BYTES_V1`].
    SourceBytes {
        /// Observed size in bytes.
        observed: usize,
    },
    /// Bracket nesting is deeper than [`MAX_NESTING_DEPTH_V1`].
    BracketDepth {
        /// Observed depth.
        observed: u32,
    },
    /// Indentation nesting is deeper than [`MAX_NESTING_DEPTH_V1`].
    IndentDepth {
        /// Observed depth.
        observed: u32,
    },
    /// A prefix-operator run is longer than [`MAX_PREFIX_OPERATOR_RUN_V1`].
    PrefixOperatorRun {
        /// Observed run length.
        observed: u32,
    },
}

impl SourceLimitViolation {
    /// Diagnostic message describing the violated limit and its bound.
    #[must_use]
    pub fn message(self) -> String {
        match self {
            Self::SourceBytes { observed } => format!(
                "cannot analyze file: {observed} bytes exceeds the {MAX_SOURCE_BYTES_V1}-byte source limit"
            ),
            Self::BracketDepth { observed } => format!(
                "cannot analyze file: bracket nesting depth {observed} exceeds the limit of {MAX_NESTING_DEPTH_V1}"
            ),
            Self::IndentDepth { observed } => format!(
                "cannot analyze file: indentation depth {observed} exceeds the limit of {MAX_NESTING_DEPTH_V1}"
            ),
            Self::PrefixOperatorRun { observed } => format!(
                "cannot analyze file: a run of {observed} prefix operators exceeds the limit of {MAX_PREFIX_OPERATOR_RUN_V1}"
            ),
        }
    }
}

/// Rejects an encoded source that is too large to analyze.
///
/// This runs before decoding so a hostile file is never materialized as a
/// `String`.
pub const fn check_source_bytes(bytes: &[u8]) -> Result<(), SourceLimitViolation> {
    if bytes.len() > MAX_SOURCE_BYTES_V1 {
        return Err(SourceLimitViolation::SourceBytes {
            observed: bytes.len(),
        });
    }
    Ok(())
}

/// Rejects a token stream whose nesting would drive the parser past the stack.
///
/// The lexer is iterative, so measuring the already-collected tokens is safe
/// even for input that the parser could not survive.
pub fn check_token_nesting(tokens: &[(Tok, TextRange)]) -> Result<(), SourceLimitViolation> {
    let mut bracket_depth: u32 = 0;
    let mut indent_depth: u32 = 0;
    let mut prefix_run: u32 = 0;

    for (token, _) in tokens {
        match token {
            Tok::Lpar | Tok::Lsqb | Tok::Lbrace => {
                bracket_depth = bracket_depth.saturating_add(1);
                if bracket_depth > MAX_NESTING_DEPTH_V1 {
                    return Err(SourceLimitViolation::BracketDepth {
                        observed: bracket_depth,
                    });
                }
            }
            Tok::Rpar | Tok::Rsqb | Tok::Rbrace => bracket_depth = bracket_depth.saturating_sub(1),
            Tok::Indent => {
                indent_depth = indent_depth.saturating_add(1);
                if indent_depth > MAX_NESTING_DEPTH_V1 {
                    return Err(SourceLimitViolation::IndentDepth {
                        observed: indent_depth,
                    });
                }
            }
            Tok::Dedent => indent_depth = indent_depth.saturating_sub(1),
            _ => {}
        }

        // A prefix run is only broken by a token that cannot continue one; the
        // brackets counted above legitimately appear inside `-(-(-x))`, so they
        // are not treated as separators.
        if matches!(token, Tok::Minus | Tok::Plus | Tok::Tilde | Tok::Not) {
            prefix_run = prefix_run.saturating_add(1);
            if prefix_run > MAX_PREFIX_OPERATOR_RUN_V1 {
                return Err(SourceLimitViolation::PrefixOperatorRun {
                    observed: prefix_run,
                });
            }
        } else if !matches!(
            token,
            Tok::Lpar
                | Tok::Lsqb
                | Tok::Lbrace
                | Tok::Rpar
                | Tok::Rsqb
                | Tok::Rbrace
                | Tok::NonLogicalNewline
                | Tok::Comment(_)
        ) {
            prefix_run = 0;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use rustpython_parser::Mode;
    use rustpython_parser::lexer::lex;

    use super::*;

    fn tokens_of(text: &str) -> Vec<(Tok, TextRange)> {
        let mut tokens = Vec::new();
        for item in lex(text, Mode::Module) {
            match item {
                Ok(token) => tokens.push(token),
                Err(_) => break,
            }
        }
        tokens
    }

    #[test]
    fn ordinary_manim_source_is_accepted() {
        let text = "from manim import *\n\n\nclass Demo(Scene):\n    def construct(self):\n        group = VGroup(Square(), Circle())\n        self.play(FadeIn(group), run_time=-1.0)\n";
        assert!(check_source_bytes(text.as_bytes()).is_ok());
        assert!(check_token_nesting(&tokens_of(text)).is_ok());
    }

    #[test]
    fn deeply_nested_calls_are_rejected_before_parsing() {
        let depth = usize::try_from(MAX_NESTING_DEPTH_V1).expect("depth fits usize") + 1;
        let text = format!("y = {}{}\n", "VGroup(".repeat(depth), ")".repeat(depth));
        let error = check_token_nesting(&tokens_of(&text)).expect_err("depth is over the limit");
        assert!(matches!(error, SourceLimitViolation::BracketDepth { .. }));
    }

    #[test]
    fn deeply_nested_blocks_are_rejected_before_parsing() {
        let depth = usize::try_from(MAX_NESTING_DEPTH_V1).expect("depth fits usize") + 1;
        let mut text = String::from("x = 1\n");
        for level in 0..depth {
            text.push_str(&"    ".repeat(level));
            text.push_str("if True:\n");
        }
        text.push_str(&"    ".repeat(depth));
        text.push_str("pass\n");
        let error = check_token_nesting(&tokens_of(&text)).expect_err("depth is over the limit");
        assert!(matches!(error, SourceLimitViolation::IndentDepth { .. }));
    }

    #[test]
    fn long_prefix_operator_runs_are_rejected_before_parsing() {
        let run = usize::try_from(MAX_PREFIX_OPERATOR_RUN_V1).expect("run fits usize") + 1;
        for source in [
            format!("x = {}1\n", "-".repeat(run)),
            format!("x = {}True\n", "not ".repeat(run)),
            format!("x = {}1\n", "~".repeat(run)),
        ] {
            let error =
                check_token_nesting(&tokens_of(&source)).expect_err("run is over the limit");
            assert!(matches!(
                error,
                SourceLimitViolation::PrefixOperatorRun { .. }
            ));
        }
    }

    #[test]
    fn line_continuations_do_not_hide_a_prefix_run() {
        let run = usize::try_from(MAX_PREFIX_OPERATOR_RUN_V1).expect("run fits usize") + 1;
        let text = format!("x = {}1\n", "-\\\n".repeat(run));
        let error = check_token_nesting(&tokens_of(&text)).expect_err("run is over the limit");
        assert!(matches!(
            error,
            SourceLimitViolation::PrefixOperatorRun { .. }
        ));
    }

    #[test]
    fn separate_operators_do_not_accumulate_into_one_run() {
        let mut text = String::from("total = 0\n");
        for _ in 0..MAX_PREFIX_OPERATOR_RUN_V1 * 4 {
            text.push_str("total = total - 1\n");
        }
        assert!(check_token_nesting(&tokens_of(&text)).is_ok());
    }

    #[test]
    fn oversized_sources_are_rejected_without_decoding() {
        let bytes = vec![b'#'; MAX_SOURCE_BYTES_V1 + 1];
        let error = check_source_bytes(&bytes).expect_err("size is over the limit");
        assert!(matches!(error, SourceLimitViolation::SourceBytes { .. }));
    }
}
