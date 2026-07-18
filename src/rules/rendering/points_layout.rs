//! `MLR112`: raw `.points` access assuming a fixed per-curve layout
//! (DESIGN §7.2, §3.6).
//!
//! Verified against the sibling Manim checkout: the Cairo `VMobject`
//! stores cubic Béziers at 4 points per curve
//! (`n_points_per_cubic_curve = 4`, `vectorized_mobject.py`), the
//! `OpenGLVMobject` quadratic Béziers at 3 points per curve
//! (`n_points_per_curve = 3`, `opengl_vectorized_mobject.py`). A literal
//! `self.points.reshape((-1, 4, 3))` or a 4-stride slice of `self.points`
//! therefore regroups garbage under OpenGL, and the 3-point forms break
//! under Cairo.
//!
//! Detection is deliberately narrow (DESIGN §3.6: "raw access is only
//! strongly diagnosed when shape and target renderer are definite"): the
//! receiver is the literal `self` of a method of a confirmed `VMobject`
//! subclass, the shape / stride is a literal, and an active profile
//! targets the renderer whose layout contradicts the assumption.

use std::collections::BTreeMap;

use rustpython_parser::ast::{self, Ranged};
use rustpython_parser::text_size::TextRange;
use serde_json::json;

use crate::config::model::Renderer;
use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::rules::base::{Rule, RuleContext};

use super::{VmClass, build_diagnostic, each_statement, renderer_profile_names};

const MLR112: RuleMetadata = RuleMetadata {
    id: "MLR112",
    summary: "Raw .points access assumes one renderer's fixed points-per-curve layout",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 4,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

/// One detected layout assumption.
struct LayoutAssumption {
    range: TextRange,
    /// Assumed points per curve (4 = Cairo cubic, 3 = OpenGL quadratic).
    per_curve: i64,
    /// How the assumption was written (`reshape` / `slice-stride`).
    form: &'static str,
}

pub(super) struct FixedPointLayoutAssumption;

impl Rule for FixedPointLayoutAssumption {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR112
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let cairo_profiles = renderer_profile_names(context, Renderer::Cairo);
        let opengl_profiles = renderer_profile_names(context, Renderer::Opengl);
        let mut diagnostics = Vec::new();
        for record in index.classes.values() {
            // Only confirmed VMobject subclasses carry the layout facts.
            if super::classify_class(profile, index, &record.qualified_name) != VmClass::Vectorized
            {
                continue;
            }
            let file = context.sources().file(record.file);
            let Some(module) = file.ast() else {
                continue;
            };
            // `self` rebound anywhere in the file makes the receiver
            // untrustworthy: silence (conservative).
            if file_rebinds_self(&module.body) {
                continue;
            }
            let mut assumptions: Vec<LayoutAssumption> = Vec::new();
            each_statement(&module.body, &mut |stmt| {
                if let ast::Stmt::ClassDef(def) = stmt {
                    if def.range() == record.range {
                        assumptions = class_body_assumptions(&def.body);
                    }
                }
            });
            for assumption in assumptions {
                // The assumption breaks under the *other* renderer; fire
                // only when an active profile targets it (DESIGN §15.8).
                let (assumed, breaks_under, applicable) = if assumption.per_curve == 4 {
                    ("Cairo (4-point cubic)", Renderer::Opengl, &opengl_profiles)
                } else {
                    (
                        "OpenGL (3-point quadratic)",
                        Renderer::Cairo,
                        &cairo_profiles,
                    )
                };
                if applicable.is_empty() {
                    continue;
                }
                let mut evidence = BTreeMap::new();
                evidence.insert("class".to_owned(), json!(record.qualified_name.clone()));
                evidence.insert("points_per_curve".to_owned(), json!(assumption.per_curve));
                evidence.insert("form".to_owned(), json!(assumption.form));
                diagnostics.push(build_diagnostic(
                    &MLR112,
                    file,
                    assumption.range,
                    Confidence::High,
                    format!(
                        "Raw `.points` access hard-codes the {assumed} layout, but this \
                         run targets the {breaks_under} renderer ({profiles}) which \
                         stores a different number of points per curve — use the \
                         public path APIs instead",
                        profiles = applicable.join(", "),
                    ),
                    "VMobject geometry storage is renderer-specific (DESIGN 3.6): \
                     Cairo groups cubic Bezier curves as 4 points per curve, OpenGL \
                     quadratic curves as 3. Reshaping or striding `.points` with a \
                     literal per-curve count silently regroups the wrong points under \
                     the other renderer. Public APIs such as get_curves(), \
                     get_nth_curve_points(), set_points_as_corners(), and \
                     point_from_proportion() are layout-independent.",
                    evidence,
                    applicable.clone(),
                    None,
                ));
            }
        }
        diagnostics
    }
}

/// Whether any non-parameter binding in the file targets the name `self`.
fn file_rebinds_self(body: &[ast::Stmt]) -> bool {
    let mut rebinds = false;
    each_statement(body, &mut |stmt| {
        let mut names = std::collections::BTreeSet::new();
        match stmt {
            ast::Stmt::Assign(assign) => {
                for target in &assign.targets {
                    super::collect_target_names(target, &mut names);
                }
            }
            ast::Stmt::AnnAssign(assign) => super::collect_target_names(&assign.target, &mut names),
            ast::Stmt::AugAssign(assign) => super::collect_target_names(&assign.target, &mut names),
            ast::Stmt::For(inner) => super::collect_target_names(&inner.target, &mut names),
            ast::Stmt::AsyncFor(inner) => super::collect_target_names(&inner.target, &mut names),
            ast::Stmt::With(inner) => {
                for item in &inner.items {
                    if let Some(vars) = &item.optional_vars {
                        super::collect_target_names(vars, &mut names);
                    }
                }
            }
            ast::Stmt::AsyncWith(inner) => {
                for item in &inner.items {
                    if let Some(vars) = &item.optional_vars {
                        super::collect_target_names(vars, &mut names);
                    }
                }
            }
            _ => {}
        }
        rebinds |= names.contains("self");
    });
    rebinds
}

/// Collects layout assumptions from a class body: direct methods and
/// their nested functions, but never nested `class` bodies (their `self`
/// is a different instance). A nested function re-binding a parameter
/// named `self` poisons the whole class (conservative silence).
fn class_body_assumptions(body: &[ast::Stmt]) -> Vec<LayoutAssumption> {
    fn walk_stmts(
        stmts: &[ast::Stmt],
        function_depth: u32,
        poisoned: &mut bool,
        assumptions: &mut Vec<LayoutAssumption>,
    ) {
        for stmt in stmts {
            if *poisoned {
                return;
            }
            for expr in statement_exprs(stmt) {
                walk_expr(expr, &mut |candidate| {
                    if let Some(assumption) = layout_assumption(candidate) {
                        assumptions.push(assumption);
                    }
                });
            }
            match stmt {
                ast::Stmt::FunctionDef(def) => {
                    if function_depth > 0 && binds_self_param(&def.args) {
                        *poisoned = true;
                        return;
                    }
                    walk_stmts(&def.body, function_depth + 1, poisoned, assumptions);
                }
                ast::Stmt::AsyncFunctionDef(def) => {
                    if function_depth > 0 && binds_self_param(&def.args) {
                        *poisoned = true;
                        return;
                    }
                    walk_stmts(&def.body, function_depth + 1, poisoned, assumptions);
                }
                // Nested classes own a different `self`.
                #[allow(clippy::match_same_arms, reason = "deliberate: never descend")]
                ast::Stmt::ClassDef(_) => {}
                ast::Stmt::If(inner) => {
                    walk_stmts(&inner.body, function_depth, poisoned, assumptions);
                    walk_stmts(&inner.orelse, function_depth, poisoned, assumptions);
                }
                ast::Stmt::While(inner) => {
                    walk_stmts(&inner.body, function_depth, poisoned, assumptions);
                    walk_stmts(&inner.orelse, function_depth, poisoned, assumptions);
                }
                ast::Stmt::For(inner) => {
                    walk_stmts(&inner.body, function_depth, poisoned, assumptions);
                    walk_stmts(&inner.orelse, function_depth, poisoned, assumptions);
                }
                ast::Stmt::AsyncFor(inner) => {
                    walk_stmts(&inner.body, function_depth, poisoned, assumptions);
                    walk_stmts(&inner.orelse, function_depth, poisoned, assumptions);
                }
                ast::Stmt::With(inner) => {
                    walk_stmts(&inner.body, function_depth, poisoned, assumptions);
                }
                ast::Stmt::AsyncWith(inner) => {
                    walk_stmts(&inner.body, function_depth, poisoned, assumptions);
                }
                ast::Stmt::Try(inner) => {
                    walk_stmts(&inner.body, function_depth, poisoned, assumptions);
                    for handler in &inner.handlers {
                        let ast::ExceptHandler::ExceptHandler(handler) = handler;
                        walk_stmts(&handler.body, function_depth, poisoned, assumptions);
                    }
                    walk_stmts(&inner.orelse, function_depth, poisoned, assumptions);
                    walk_stmts(&inner.finalbody, function_depth, poisoned, assumptions);
                }
                ast::Stmt::TryStar(inner) => {
                    walk_stmts(&inner.body, function_depth, poisoned, assumptions);
                    for handler in &inner.handlers {
                        let ast::ExceptHandler::ExceptHandler(handler) = handler;
                        walk_stmts(&handler.body, function_depth, poisoned, assumptions);
                    }
                    walk_stmts(&inner.orelse, function_depth, poisoned, assumptions);
                    walk_stmts(&inner.finalbody, function_depth, poisoned, assumptions);
                }
                ast::Stmt::Match(inner) => {
                    for case in &inner.cases {
                        walk_stmts(&case.body, function_depth, poisoned, assumptions);
                    }
                }
                _ => {}
            }
        }
    }
    let mut assumptions = Vec::new();
    let mut poisoned = false;
    walk_stmts(body, 0, &mut poisoned, &mut assumptions);
    if poisoned { Vec::new() } else { assumptions }
}

/// Whether an argument list declares a parameter literally named `self`.
fn binds_self_param(args: &ast::Arguments) -> bool {
    args.posonlyargs
        .iter()
        .chain(&args.args)
        .chain(&args.kwonlyargs)
        .any(|arg| arg.def.arg.as_str() == "self")
        || args
            .vararg
            .as_ref()
            .is_some_and(|arg| arg.arg.as_str() == "self")
        || args
            .kwarg
            .as_ref()
            .is_some_and(|arg| arg.arg.as_str() == "self")
}

/// The direct expressions of one statement (bodies are handled by
/// [`each_statement`]).
fn statement_exprs(stmt: &ast::Stmt) -> Vec<&ast::Expr> {
    match stmt {
        ast::Stmt::Expr(inner) => vec![&inner.value],
        ast::Stmt::Return(inner) => inner.value.as_deref().into_iter().collect(),
        ast::Stmt::Assign(inner) => {
            let mut exprs: Vec<&ast::Expr> = inner.targets.iter().collect();
            exprs.push(&inner.value);
            exprs
        }
        ast::Stmt::AugAssign(inner) => vec![&inner.target, &inner.value],
        ast::Stmt::AnnAssign(inner) => {
            let mut exprs = vec![inner.target.as_ref()];
            if let Some(value) = &inner.value {
                exprs.push(value);
            }
            exprs
        }
        ast::Stmt::If(inner) => vec![&inner.test],
        ast::Stmt::While(inner) => vec![&inner.test],
        ast::Stmt::For(inner) => vec![&inner.target, &inner.iter],
        ast::Stmt::AsyncFor(inner) => vec![&inner.target, &inner.iter],
        ast::Stmt::With(inner) => inner.items.iter().map(|item| &item.context_expr).collect(),
        ast::Stmt::AsyncWith(inner) => inner.items.iter().map(|item| &item.context_expr).collect(),
        ast::Stmt::Assert(inner) => {
            let mut exprs = vec![inner.test.as_ref()];
            if let Some(message) = &inner.msg {
                exprs.push(message);
            }
            exprs
        }
        ast::Stmt::Raise(inner) => {
            let mut exprs = Vec::new();
            if let Some(exc) = &inner.exc {
                exprs.push(exc.as_ref());
            }
            if let Some(cause) = &inner.cause {
                exprs.push(cause.as_ref());
            }
            exprs
        }
        ast::Stmt::Delete(inner) => inner.targets.iter().collect(),
        ast::Stmt::Match(inner) => vec![&inner.subject],
        _ => Vec::new(),
    }
}

/// Depth-first visit over an expression and its subexpressions.
#[allow(clippy::too_many_lines, reason = "one arm per expression kind")]
fn walk_expr<'a>(expr: &'a ast::Expr, visit: &mut dyn FnMut(&'a ast::Expr)) {
    visit(expr);
    match expr {
        ast::Expr::BoolOp(inner) => {
            for value in &inner.values {
                walk_expr(value, visit);
            }
        }
        ast::Expr::NamedExpr(inner) => {
            walk_expr(&inner.target, visit);
            walk_expr(&inner.value, visit);
        }
        ast::Expr::BinOp(inner) => {
            walk_expr(&inner.left, visit);
            walk_expr(&inner.right, visit);
        }
        ast::Expr::UnaryOp(inner) => walk_expr(&inner.operand, visit),
        ast::Expr::Lambda(inner) => walk_expr(&inner.body, visit),
        ast::Expr::IfExp(inner) => {
            walk_expr(&inner.test, visit);
            walk_expr(&inner.body, visit);
            walk_expr(&inner.orelse, visit);
        }
        ast::Expr::Dict(inner) => {
            for key in inner.keys.iter().flatten() {
                walk_expr(key, visit);
            }
            for value in &inner.values {
                walk_expr(value, visit);
            }
        }
        ast::Expr::Set(inner) => {
            for element in &inner.elts {
                walk_expr(element, visit);
            }
        }
        ast::Expr::ListComp(inner) => {
            walk_expr(&inner.elt, visit);
            walk_comprehensions(&inner.generators, visit);
        }
        ast::Expr::SetComp(inner) => {
            walk_expr(&inner.elt, visit);
            walk_comprehensions(&inner.generators, visit);
        }
        ast::Expr::DictComp(inner) => {
            walk_expr(&inner.key, visit);
            walk_expr(&inner.value, visit);
            walk_comprehensions(&inner.generators, visit);
        }
        ast::Expr::GeneratorExp(inner) => {
            walk_expr(&inner.elt, visit);
            walk_comprehensions(&inner.generators, visit);
        }
        ast::Expr::Await(inner) => walk_expr(&inner.value, visit),
        ast::Expr::Yield(inner) => {
            if let Some(value) = &inner.value {
                walk_expr(value, visit);
            }
        }
        ast::Expr::YieldFrom(inner) => walk_expr(&inner.value, visit),
        ast::Expr::Compare(inner) => {
            walk_expr(&inner.left, visit);
            for comparator in &inner.comparators {
                walk_expr(comparator, visit);
            }
        }
        ast::Expr::Call(inner) => {
            walk_expr(&inner.func, visit);
            for argument in &inner.args {
                walk_expr(argument, visit);
            }
            for keyword in &inner.keywords {
                walk_expr(&keyword.value, visit);
            }
        }
        ast::Expr::FormattedValue(inner) => walk_expr(&inner.value, visit),
        ast::Expr::JoinedStr(inner) => {
            for value in &inner.values {
                walk_expr(value, visit);
            }
        }
        ast::Expr::Attribute(inner) => walk_expr(&inner.value, visit),
        ast::Expr::Subscript(inner) => {
            walk_expr(&inner.value, visit);
            walk_expr(&inner.slice, visit);
        }
        ast::Expr::Starred(inner) => walk_expr(&inner.value, visit),
        ast::Expr::List(inner) => {
            for element in &inner.elts {
                walk_expr(element, visit);
            }
        }
        ast::Expr::Tuple(inner) => {
            for element in &inner.elts {
                walk_expr(element, visit);
            }
        }
        ast::Expr::Slice(inner) => {
            for part in [&inner.lower, &inner.upper, &inner.step]
                .into_iter()
                .flatten()
            {
                walk_expr(part, visit);
            }
        }
        ast::Expr::Constant(_) | ast::Expr::Name(_) => {}
    }
}

fn walk_comprehensions<'a>(
    generators: &'a [ast::Comprehension],
    visit: &mut dyn FnMut(&'a ast::Expr),
) {
    for generator in generators {
        walk_expr(&generator.target, visit);
        walk_expr(&generator.iter, visit);
        for condition in &generator.ifs {
            walk_expr(condition, visit);
        }
    }
}

/// Whether `expr` is the attribute chain `self.points`.
fn is_self_points(expr: &ast::Expr) -> bool {
    let ast::Expr::Attribute(attribute) = expr else {
        return false;
    };
    if attribute.attr.as_str() != "points" {
        return false;
    }
    matches!(attribute.value.as_ref(), ast::Expr::Name(name) if name.id.as_str() == "self")
}

/// The integer value of a literal (possibly negated) constant.
fn literal_int(expr: &ast::Expr) -> Option<i64> {
    match expr {
        ast::Expr::Constant(constant) => match &constant.value {
            ast::Constant::Int(value) => value.to_string().parse().ok(),
            _ => None,
        },
        ast::Expr::UnaryOp(unary) if matches!(unary.op, ast::UnaryOp::USub) => {
            literal_int(&unary.operand).map(|value| -value)
        }
        _ => None,
    }
}

/// Classifies one expression as a literal per-curve layout assumption.
fn layout_assumption(expr: &ast::Expr) -> Option<LayoutAssumption> {
    match expr {
        // self.points.reshape((-1, N, 3)) / self.points.reshape(-1, N, 3)
        ast::Expr::Call(call) => {
            let ast::Expr::Attribute(callee) = call.func.as_ref() else {
                return None;
            };
            if callee.attr.as_str() != "reshape" || !is_self_points(&callee.value) {
                return None;
            }
            if !call.keywords.is_empty() {
                return None;
            }
            let dims: Vec<i64> = match call.args.as_slice() {
                [ast::Expr::Tuple(tuple)] => tuple
                    .elts
                    .iter()
                    .map(literal_int)
                    .collect::<Option<Vec<i64>>>()?,
                args => args.iter().map(literal_int).collect::<Option<Vec<i64>>>()?,
            };
            let [head, per_curve, width] = dims.as_slice() else {
                return None;
            };
            if *head != -1 || *width != 3 || !matches!(per_curve, 3 | 4) {
                return None;
            }
            Some(LayoutAssumption {
                range: expr.range(),
                per_curve: *per_curve,
                form: "reshape",
            })
        }
        // self.points[a::N] with literal stride N in {3, 4}.
        ast::Expr::Subscript(subscript) => {
            if !is_self_points(&subscript.value) {
                return None;
            }
            let ast::Expr::Slice(slice) = subscript.slice.as_ref() else {
                return None;
            };
            let step = literal_int(slice.step.as_deref()?)?;
            if !matches!(step, 3 | 4) {
                return None;
            }
            Some(LayoutAssumption {
                range: expr.range(),
                per_curve: step,
                form: "slice-stride",
            })
        }
        _ => None,
    }
}
