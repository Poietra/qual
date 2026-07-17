//! `MLR114`: a literal points array passed to a `set_points`-family
//! method on a confirmed `VMobject` is not N×3.
//!
//! Only a direct list/tuple-of-lists literal whose rows are all numeric
//! literals is judged; names, `np.array(...)` wrappers, and anything with
//! an unresolved element bail to silence. A project override of the
//! method also bails: only the curated Manim implementations are trusted
//! to require 3-component points.

use std::collections::BTreeMap;

use rustpython_parser::ast;
use serde_json::json;

use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::frontend::index::ReceiverKind;
use crate::rules::base::{Rule, RuleContext};
use crate::rules::rendering::VmClass;
use crate::source::SourceFile;

/// The `set_points` family: every member stores its argument as the
/// `VMobject`'s `(N, 3)` points (directly or through
/// `set_anchors_and_handles`).
const POINT_METHODS: &[&str] = &[
    "add_points_as_corners",
    "append_points",
    "set_points",
    "set_points_as_corners",
    "set_points_smoothly",
];

const MLR114: RuleMetadata = RuleMetadata {
    id: "MLR114",
    summary: "Literal points array is not N x 3",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::High,
    implementation_phase: 2,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

pub(super) struct NonPoint3Literal;

impl Rule for NonPoint3Literal {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR114
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let profiles = context.config().active_profile_names();
        let mut diagnostics = Vec::new();
        for call in &context.qualified_calls().calls {
            if !matches!(call.receiver, ReceiverKind::KnownInstance(_))
                || call.candidates.is_empty()
                || call.has_star_args
            {
                continue;
            }
            // Every candidate must be the same set_points-family method on
            // a confirmed vectorized class, with no project override.
            let mut method_name: Option<&str> = None;
            let mut confirmed = true;
            for candidate in &call.candidates {
                let Some((class, method)) = candidate.rsplit_once('.') else {
                    confirmed = false;
                    break;
                };
                if !POINT_METHODS.contains(&method)
                    || method_name.is_some_and(|seen| seen != method)
                    || index.classes.contains_key(class)
                    || super::classify_class(profile, index, class) != VmClass::Vectorized
                {
                    confirmed = false;
                    break;
                }
                method_name = Some(method);
            }
            let (true, Some(method)) = (confirmed, method_name) else {
                continue;
            };
            let Some(argument) = call
                .positional(0)
                .filter(|argument| argument.keyword.is_none())
            else {
                continue;
            };
            let file = context.sources().file(call.file);
            let Some(rows) = literal_row_lengths(file, argument.range) else {
                continue;
            };
            let Some(bad) = rows.iter().position(|len| *len != 3) else {
                continue;
            };
            let mut evidence = BTreeMap::new();
            evidence.insert("method".to_owned(), json!(method));
            evidence.insert("row_lengths".to_owned(), json!(rows));
            evidence.insert("expected_components".to_owned(), json!(3));
            diagnostics.push(super::build_diagnostic(
                &MLR114,
                file,
                argument.range,
                Confidence::High,
                format!(
                    "`.{method}()` expects an (N, 3) points array; row {row} has \
                     {len} component{plural}",
                    row = bad + 1,
                    len = rows[bad],
                    plural = if rows[bad] == 1 { "" } else { "s" },
                ),
                "VMobject points are 3-component rows: the renderers regroup them \
                 into Bézier curves (4 points per cubic curve under Cairo, 3 per \
                 quadratic under OpenGL) by reshaping to (-1, 3), and geometry \
                 operations broadcast against 3-component direction vectors. A \
                 row with any other length corrupts that grouping or raises at \
                 the first reshape. Append a z component (usually 0) to each \
                 point.",
                evidence,
                profiles.clone(),
                None,
            ));
        }
        diagnostics
    }
}

/// Row lengths of a literal list/tuple-of-lists points expression.
///
/// `None` unless the whole expression is a list/tuple literal whose
/// elements are all list/tuple literals of numeric literals — anything
/// unresolved (names, calls, arithmetic, splats) bails.
fn literal_row_lengths(
    file: &SourceFile,
    range: rustpython_parser::text_size::TextRange,
) -> Option<Vec<usize>> {
    let text = file.slice(range);
    let parsed = rustpython_parser::parse(text, rustpython_parser::Mode::Expression, "<points>");
    let Ok(ast::Mod::Expression(module)) = parsed else {
        return None;
    };
    let rows = sequence_elements(&module.body)?;
    let mut lengths = Vec::with_capacity(rows.len());
    for row in rows {
        let components = sequence_elements(row)?;
        if !components.iter().all(is_numeric_literal) {
            return None;
        }
        lengths.push(components.len());
    }
    Some(lengths)
}

/// The elements of a list/tuple literal, `None` for any other expression.
fn sequence_elements(expr: &ast::Expr) -> Option<&[ast::Expr]> {
    match expr {
        ast::Expr::List(list) => Some(&list.elts),
        ast::Expr::Tuple(tuple) => Some(&tuple.elts),
        _ => None,
    }
}

/// Whether the expression is a plain numeric literal (int or float, with
/// an optional leading sign).
fn is_numeric_literal(expr: &ast::Expr) -> bool {
    match expr {
        ast::Expr::Constant(constant) => matches!(
            constant.value,
            ast::Constant::Int(_) | ast::Constant::Float(_)
        ),
        ast::Expr::UnaryOp(unary) => {
            matches!(unary.op, ast::UnaryOp::USub | ast::UnaryOp::UAdd)
                && is_numeric_literal(&unary.operand)
        }
        _ => false,
    }
}
