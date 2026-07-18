//! `MLD301`: updater applies a fixed per-frame step without `dt` scaling
//! (DESIGN §7.4).
//!
//! Manim calls an updater as `(mobject, dt)` only when the callback
//! declares a parameter literally named `dt`; otherwise the callback runs
//! once per frame with no time information (DESIGN §3.3). A relative
//! mutation with a fixed step (`shift`, `rotate`, `scale`,
//! `increment_value`) inside such a callback therefore advances per
//! *frame*, not per second — the rendered motion speed changes with the
//! profile frame rate.
//!
//! The DESIGN §7.4 prose triple is the exact spec:
//!
//! ```python
//! dot.add_updater(lambda m: m.next_to(driver))       # absolute: fine
//! dot.add_updater(lambda m: m.shift(0.1 * RIGHT))    # fires
//! dot.add_updater(lambda m, dt: m.shift(dt * RIGHT)) # time-based: fine
//! ```
//!
//! Conservative gates: only `Mobject.add_updater` registrations whose
//! callback is a lambda or an unambiguous project `def`; only mutation
//! calls on the updater's own mobject parameter; only arguments composed
//! entirely of numeric literals and knowledge-resolved Manim constants
//! (`RIGHT`, `PI`, ...). Anything else — tracker reads, closure variables,
//! unknown names — stays silent.

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::ast::{self, Ranged};
use rustpython_parser::text_size::TextRange;
use serde_json::json;

use crate::cost::contexts::resolve_candidate;
use crate::diagnostic::{Confidence, Diagnostic, RelatedLocation, RuleMetadata, Severity};
use crate::frontend::index::{ArgShape, QualifiedCall};
use crate::frontend::statements::{
    FileBindingFacts, each_statement, lambda_at, statement_exprs, unique_function_def, walk_expr,
};
use crate::knowledge::{KnowledgeProfile, SymbolKind};
use crate::rules::base::{Rule, RuleContext};
use crate::source::{FileId, SourceFile};

use super::build_diagnostic;

/// Canonical id of the mobject-level updater registration.
const MOBJECT_ADD_UPDATER: &str = "manim.mobject.mobject.Mobject.add_updater";

/// Relative mutators whose fixed-step per-frame application is
/// FPS-dependent.
const RELATIVE_MUTATORS: [&str; 4] = ["increment_value", "rotate", "scale", "shift"];

pub(super) const MLD301: RuleMetadata = RuleMetadata {
    id: "MLD301",
    summary: "Updater applies a fixed per-frame step without dt scaling (FPS-dependent motion)",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 4,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

pub(super) struct FpsDependentUpdaterMotion;

impl Rule for FpsDependentUpdaterMotion {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLD301
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(knowledge) = context.knowledge() else {
            return Vec::new();
        };
        let profiles = context.config().active_profile_names();
        let mut reported: BTreeSet<(FileId, u32, u32)> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for call in &context.qualified_calls().calls {
            if !is_mobject_add_updater(knowledge, call) {
                continue;
            }
            let Some(argument) = call.keyword("update_function").or_else(|| {
                if call.has_star_args {
                    None
                } else {
                    call.positional(0)
                }
            }) else {
                continue;
            };
            let file = context.sources().file(call.file);
            let Some(callback) = resolve_callback_body(file, argument.range, &argument.shape)
            else {
                continue;
            };
            // A `dt` parameter makes Manim pass frame time; the callback
            // owns FPS-independence itself (fine per the DESIGN triple).
            if callback.declares_dt {
                continue;
            }
            let Some(param) = callback.mobject_param.clone() else {
                continue;
            };
            let Some(bindings) = context.binding_facts().file(call.file) else {
                continue;
            };
            for mutation in fixed_step_mutations(&callback, &param, bindings, knowledge) {
                let key = (
                    call.file,
                    mutation.range.start().into(),
                    mutation.range.end().into(),
                );
                if !reported.insert(key) {
                    continue;
                }
                let mut evidence = BTreeMap::new();
                evidence.insert("method".to_owned(), json!(mutation.method));
                evidence.insert("callback".to_owned(), json!(callback.label.clone()));
                evidence.insert("declares_dt".to_owned(), json!(false));
                let mut diagnostic = build_diagnostic(
                    &MLD301,
                    file,
                    mutation.range,
                    Confidence::High,
                    format!(
                        "Updater {label} applies `{method}` with a fixed step every frame \
                         but declares no `dt` parameter; the motion speed depends on the \
                         profile frame rate",
                        label = callback.label,
                        method = mutation.method,
                    ),
                    "Manim calls an updater as (mobject, dt) only when the callback \
                     declares a parameter literally named dt; otherwise it runs once per \
                     frame with no time information. A relative mutation with a fixed \
                     step then advances per frame, so the same scene moves twice as fast \
                     at 60 FPS as at 30 FPS. Declare (mobject, dt) and scale the step by \
                     dt, or use an absolute update such as next_to.",
                    evidence,
                    profiles.clone(),
                    None,
                );
                diagnostic.related_locations.push(RelatedLocation {
                    path: file.relative_path().to_owned(),
                    span: file.span_of_range(call.call_range),
                    message: "updater registered here".to_owned(),
                });
                diagnostics.push(diagnostic);
            }
        }
        diagnostics
    }
}

/// Whether every candidate of the call resolves to
/// `Mobject.add_updater` through the curated base chain (Scene-level
/// updaters have a different `(dt)` contract and are excluded).
fn is_mobject_add_updater(knowledge: &KnowledgeProfile, call: &QualifiedCall) -> bool {
    if call.candidates.is_empty() {
        return false;
    }
    call.candidates.iter().all(|candidate| {
        resolve_candidate(knowledge, candidate)
            .is_some_and(|(canonical, _)| canonical == MOBJECT_ADD_UPDATER)
    })
}

/// The resolved callback of one registration.
struct CallbackBody<'a> {
    /// `"lambda"` or the `def` name, for messages.
    label: String,
    /// Name of the first positional parameter (the mobject), if any.
    mobject_param: Option<String>,
    /// Whether any parameter is literally named `dt`.
    declares_dt: bool,
    /// Body statements (`def`) — empty for lambdas.
    stmts: &'a [ast::Stmt],
    /// Body expression (lambda) — `None` for defs.
    expr: Option<&'a ast::Expr>,
}

/// Resolves the callback argument to its AST body via the frontend's
/// fact-anchored node queries.
///
/// Lambdas are located by their exact byte range; named callbacks are
/// accepted only when exactly one `def` with that name exists in the whole
/// file (nested defs included) — ambiguity means silence.
fn resolve_callback_body<'a>(
    file: &'a SourceFile,
    range: TextRange,
    shape: &ArgShape,
) -> Option<CallbackBody<'a>> {
    match shape {
        ArgShape::Lambda => {
            let lambda = lambda_at(file, range)?;
            Some(CallbackBody {
                label: "lambda".to_owned(),
                mobject_param: first_positional_param(&lambda.args),
                declares_dt: declares_dt(&lambda.args),
                stmts: &[],
                expr: Some(&lambda.body),
            })
        }
        ArgShape::Name => {
            let name = file.slice(range);
            let def = unique_function_def(file, name)?;
            Some(CallbackBody {
                label: format!("`{name}`"),
                mobject_param: first_positional_param(&def.args),
                declares_dt: declares_dt(&def.args),
                stmts: &def.body,
                expr: None,
            })
        }
        _ => None,
    }
}

fn first_positional_param(args: &ast::Arguments) -> Option<String> {
    args.posonlyargs
        .iter()
        .chain(&args.args)
        .next()
        .map(|arg| arg.def.arg.to_string())
}

fn declares_dt(args: &ast::Arguments) -> bool {
    args.posonlyargs
        .iter()
        .chain(&args.args)
        .chain(&args.kwonlyargs)
        .any(|arg| arg.def.arg.as_str() == "dt")
        || args
            .vararg
            .as_deref()
            .is_some_and(|arg| arg.arg.as_str() == "dt")
        || args
            .kwarg
            .as_deref()
            .is_some_and(|arg| arg.arg.as_str() == "dt")
}

/// One provably fixed-step relative mutation inside the callback body.
struct FixedStepMutation {
    /// The mutator method name (`shift`, ...).
    method: String,
    /// Byte range of the mutation call.
    range: TextRange,
}

/// Collects `param.shift(...)` / `rotate` / `scale` / `increment_value`
/// calls whose every argument is a literal-and-constant expression.
fn fixed_step_mutations(
    callback: &CallbackBody<'_>,
    param: &str,
    bindings: &FileBindingFacts,
    knowledge: &KnowledgeProfile,
) -> Vec<FixedStepMutation> {
    let mut mutations = Vec::new();
    let mut inspect = |expr: &ast::Expr| {
        let ast::Expr::Call(call) = expr else {
            return;
        };
        let ast::Expr::Attribute(attribute) = call.func.as_ref() else {
            return;
        };
        let ast::Expr::Name(receiver) = attribute.value.as_ref() else {
            return;
        };
        if receiver.id.as_str() != param {
            return;
        }
        let method = attribute.attr.as_str();
        if !RELATIVE_MUTATORS.contains(&method) {
            return;
        }
        if call.args.is_empty() && call.keywords.is_empty() {
            return;
        }
        // A `**kwargs` splat (keyword without a name) is never constant.
        if call.keywords.iter().any(|keyword| keyword.arg.is_none()) {
            return;
        }
        let all_constant = call
            .args
            .iter()
            .chain(call.keywords.iter().map(|keyword| &keyword.value))
            .all(|argument| is_constant_step(argument, bindings, knowledge));
        if all_constant {
            mutations.push(FixedStepMutation {
                method: method.to_owned(),
                range: call.range(),
            });
        }
    };
    if let Some(expr) = callback.expr {
        walk_expr(expr, &mut inspect);
    }
    each_statement(callback.stmts, &mut |stmt| {
        for expr in statement_exprs(stmt) {
            walk_expr(expr, &mut inspect);
        }
    });
    mutations
}

/// Whether an argument expression is provably frame-invariant and
/// tracker-independent: numeric literals and knowledge Manim constants
/// combined with `+ - * /`, unary sign, and tuple / list grouping.
fn is_constant_step(
    expr: &ast::Expr,
    bindings: &FileBindingFacts,
    knowledge: &KnowledgeProfile,
) -> bool {
    match expr {
        ast::Expr::Constant(constant) => matches!(
            constant.value,
            ast::Constant::Int(_) | ast::Constant::Float(_)
        ),
        ast::Expr::Name(name) => is_manim_constant_name(bindings, knowledge, name.id.as_str()),
        ast::Expr::Attribute(_) => is_manim_constant_attribute(bindings, knowledge, expr),
        ast::Expr::UnaryOp(inner) => {
            matches!(inner.op, ast::UnaryOp::UAdd | ast::UnaryOp::USub)
                && is_constant_step(&inner.operand, bindings, knowledge)
        }
        ast::Expr::BinOp(inner) => {
            matches!(
                inner.op,
                ast::Operator::Add | ast::Operator::Sub | ast::Operator::Mult | ast::Operator::Div
            ) && is_constant_step(&inner.left, bindings, knowledge)
                && is_constant_step(&inner.right, bindings, knowledge)
        }
        ast::Expr::Tuple(inner) => {
            !inner.elts.is_empty()
                && inner
                    .elts
                    .iter()
                    .all(|element| is_constant_step(element, bindings, knowledge))
        }
        ast::Expr::List(inner) => {
            !inner.elts.is_empty()
                && inner
                    .elts
                    .iter()
                    .all(|element| is_constant_step(element, bindings, knowledge))
        }
        _ => false,
    }
}

/// Whether a bare name reaches a knowledge-curated Manim constant
/// (`RIGHT`, `PI`, ...) through this file's imports.
fn is_manim_constant_name(
    bindings: &FileBindingFacts,
    knowledge: &KnowledgeProfile,
    name: &str,
) -> bool {
    let export = if bindings.has_star_from("manim") && bindings.is_unshadowed_bare(name) {
        Some(name.to_owned())
    } else {
        bindings
            .resolve_parts(std::slice::from_ref(&name.to_owned()))
            .and_then(|target| target.strip_prefix("manim.").map(str::to_owned))
    };
    export.is_some_and(|export| is_constant_export(knowledge, &export))
}

/// Whether `mn.RIGHT`-style attribute access reaches a Manim constant.
fn is_manim_constant_attribute(
    bindings: &FileBindingFacts,
    knowledge: &KnowledgeProfile,
    expr: &ast::Expr,
) -> bool {
    bindings
        .resolve_expr(expr)
        .and_then(|target| target.strip_prefix("manim.").map(str::to_owned))
        .is_some_and(|export| is_constant_export(knowledge, &export))
}

fn is_constant_export(knowledge: &KnowledgeProfile, export: &str) -> bool {
    if export.contains('.') {
        return false;
    }
    knowledge
        .resolve_export(export)
        .is_some_and(|(_, entry)| entry.kind == SymbolKind::Constant)
}
