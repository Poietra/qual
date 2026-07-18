//! `MLP210`: many short sequential plays inside a fixed-count loop
//! (DESIGN §7.3).
//!
//! Cost claim: per-play begin/hash overhead × the literal trip count.
//! The per-play *partial-stream* boundary cost is deliberately **not**
//! included: no `OutputState` fact proves a per-play stream exists
//! (binding §7.3 gate — a continuous writer has no such boundary), so
//! the overhead is described qualitatively.

use std::collections::BTreeMap;

use rustpython_parser::ast::{self, Ranged};
use serde_json::{Value, json};

use crate::cost::estimator::{num_bounds_json, play_run_time};
use crate::cost::thresholds::Threshold;
use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::frontend::index::QualifiedCall;
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::values::Num;
use crate::source::SourceFile;

use super::support::{SCENE_PLAY, bound_receiver, build_diagnostic, conclusive_target};

/// `MLP210` fires only on a literal trip count of at least this many
/// plays ("many short sequential plays"; fewer plays carry negligible
/// aggregate startup cost).
pub const MLP210_LOOP_PLAYS_GATE: Threshold = Threshold {
    name: "loop-plays-gate",
    quantity: "N_plays",
    minimum: 8.0,
    rule_ids: &["MLP210"],
};

/// Metadata for [`FixedCountLoopPlays`].
pub const MLP210: RuleMetadata = RuleMetadata {
    id: "MLP210",
    summary: "Fixed-count loop issues many short sequential plays",
    default_enabled: true,
    default_severity: Severity::Info,
    minimum_confidence: Confidence::Medium,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "cost-facts"],
    supersedes: &[],
};

/// A `self.play(...)` statement that is a direct child of a `for` loop
/// with a literal trip count of [`MLP210_LOOP_PLAYS_GATE`] or more
/// iterations: every iteration pays the full play startup (animation
/// compile, hash, begin copies, cleanup) once (DESIGN §7.3 `MLP210`).
pub struct FixedCountLoopPlays;

/// The literal trip count of a `for` iterable: an int-literal `range()`
/// (negative literals via unary minus included) or a display without
/// starred elements. `None` for everything else — never a guess.
fn literal_trip_count(iter: &ast::Expr) -> Option<i64> {
    fn int_of(expr: &ast::Expr) -> Option<i64> {
        match expr {
            ast::Expr::Constant(constant) => match &constant.value {
                ast::Constant::Int(value) => i64::try_from(value).ok(),
                _ => None,
            },
            ast::Expr::UnaryOp(unary) if matches!(unary.op, ast::UnaryOp::USub) => {
                int_of(&unary.operand).map(|value| -value)
            }
            _ => None,
        }
    }
    match iter {
        ast::Expr::Call(call) => {
            let ast::Expr::Name(name) = call.func.as_ref() else {
                return None;
            };
            if name.id.as_str() != "range" || !call.keywords.is_empty() {
                return None;
            }
            let count = match call.args.as_slice() {
                [stop] => int_of(stop)?,
                [start, stop] => int_of(stop)? - int_of(start)?,
                [start, stop, stride] => {
                    let stride = int_of(stride)?;
                    if stride <= 0 {
                        return None;
                    }
                    let span = int_of(stop)? - int_of(start)?;
                    span.div_euclid(stride) + i64::from(span.rem_euclid(stride) > 0)
                }
                _ => return None,
            };
            (count > 0).then_some(count)
        }
        ast::Expr::List(display) => display_len(&display.elts),
        ast::Expr::Tuple(display) => display_len(&display.elts),
        _ => None,
    }
}

fn display_len(elements: &[ast::Expr]) -> Option<i64> {
    if elements
        .iter()
        .any(|element| matches!(element, ast::Expr::Starred(_)))
    {
        return None;
    }
    let count = i64::try_from(elements.len()).ok()?;
    (count > 0).then_some(count)
}

/// Whether the loop body contains a `break` / `continue` / `return` /
/// `raise` anywhere: the per-iteration play count is then not provable.
fn body_may_short_circuit(stmts: &[ast::Stmt]) -> bool {
    stmts.iter().any(stmt_may_short_circuit)
}

fn stmt_may_short_circuit(stmt: &ast::Stmt) -> bool {
    matches!(
        stmt,
        ast::Stmt::Break(_) | ast::Stmt::Continue(_) | ast::Stmt::Return(_) | ast::Stmt::Raise(_)
    ) || child_stmts(stmt).is_some_and(|children| children.into_iter().any(stmt_may_short_circuit))
}

/// Nested statement lists of a compound statement (`None` for simple
/// statements).
fn child_stmts(stmt: &ast::Stmt) -> Option<Vec<&ast::Stmt>> {
    let children: Vec<&ast::Stmt> = match stmt {
        ast::Stmt::For(inner) => inner.body.iter().chain(&inner.orelse).collect(),
        ast::Stmt::AsyncFor(inner) => inner.body.iter().chain(&inner.orelse).collect(),
        ast::Stmt::While(inner) => inner.body.iter().chain(&inner.orelse).collect(),
        ast::Stmt::If(inner) => inner.body.iter().chain(&inner.orelse).collect(),
        ast::Stmt::With(inner) => inner.body.iter().collect(),
        ast::Stmt::AsyncWith(inner) => inner.body.iter().collect(),
        ast::Stmt::Try(inner) => inner
            .body
            .iter()
            .chain(inner.handlers.iter().flat_map(|handler| {
                let ast::ExceptHandler::ExceptHandler(handler) = handler;
                handler.body.iter()
            }))
            .chain(&inner.orelse)
            .chain(&inner.finalbody)
            .collect(),
        ast::Stmt::Match(inner) => inner
            .cases
            .iter()
            .flat_map(|case| case.body.iter())
            .collect(),
        // Nested defs / classes do not run per iteration.
        _ => return None,
    };
    Some(children)
}

/// The innermost `for` loop whose body *directly* contains the play call
/// as an expression statement (a play behind an `if` or in a helper does
/// not run once per iteration provably).
fn enclosing_direct_loop<'a>(
    file: &'a SourceFile,
    call: &QualifiedCall,
) -> Option<&'a ast::StmtFor> {
    fn search<'a>(
        stmts: &'a [ast::Stmt],
        call: &QualifiedCall,
        found: &mut Option<&'a ast::StmtFor>,
    ) {
        for stmt in stmts {
            if found.is_some() {
                return;
            }
            if let ast::Stmt::For(for_stmt) = stmt {
                let direct = for_stmt.body.iter().any(|body_stmt| {
                    matches!(
                        body_stmt,
                        ast::Stmt::Expr(expr) if expr.value.range() == call.call_range
                    )
                });
                if direct {
                    *found = Some(for_stmt);
                    return;
                }
            }
            match stmt {
                ast::Stmt::FunctionDef(def) => search(&def.body, call, found),
                ast::Stmt::AsyncFunctionDef(def) => search(&def.body, call, found),
                ast::Stmt::ClassDef(def) => search(&def.body, call, found),
                other => {
                    if let Some(children) = child_stmts(other) {
                        for child in children {
                            search(std::slice::from_ref(child), call, found);
                        }
                    }
                }
            }
        }
    }
    let module = file.ast()?;
    let mut found: Option<&ast::StmtFor> = None;
    search(&module.body, call, &mut found);
    found
}

impl Rule for FixedCountLoopPlays {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP210
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let calls = context.qualified_calls();
        let mut diagnostics = Vec::new();
        for call in &calls.calls {
            if !bound_receiver(call) {
                continue;
            }
            let Some((canonical, _)) = conclusive_target(profile, index, call) else {
                continue;
            };
            if canonical != SCENE_PLAY {
                continue;
            }
            let file = context.sources().file(call.file);
            let Some(for_stmt) = enclosing_direct_loop(file, call) else {
                continue;
            };
            if body_may_short_circuit(&for_stmt.body) {
                continue;
            }
            let Some(count) = literal_trip_count(&for_stmt.iter) else {
                continue;
            };
            let plays = Num::int(count);
            if !MLP210_LOOP_PLAYS_GATE.confirmed_by(&plays) {
                continue;
            }
            let run_time = play_run_time(call, calls);
            let mut evidence = BTreeMap::new();
            evidence.insert("resolved".to_owned(), Value::String(SCENE_PLAY.to_owned()));
            evidence.insert("plays".to_owned(), json!(count));
            evidence.insert(
                "threshold".to_owned(),
                MLP210_LOOP_PLAYS_GATE.evidence(&plays),
            );
            evidence.insert(
                "citation".to_owned(),
                json!(MLP210_LOOP_PLAYS_GATE.citation()),
            );
            evidence.insert("run_time_seconds".to_owned(), num_bounds_json(&run_time));
            evidence.insert(
                "per_play_overhead".to_owned(),
                json!(
                    "animation compile + hash + begin starting copies + cleanup, once \
                     per play; per-play partial-stream cost omitted (no per-play \
                     output-stream fact)"
                ),
            );
            diagnostics.push(build_diagnostic(
                &MLP210,
                context,
                file,
                call.call_range,
                format!(
                    "This loop issues {count} sequential plays: each one pays the \
                     full play startup (animation compile, hash, begin copies, \
                     cleanup) before rendering a single frame. If the {count} \
                     animations can run together, one play of an AnimationGroup \
                     (or LaggedStart) pays the startup once.",
                ),
                "Every Scene.play compiles its animations, hashes them for the \
                 cache, deep-copies the starting mobjects, and runs cleanup — a \
                 fixed cost independent of the play's duration (DESIGN 4.3 \
                 Animation begin). A literal-count loop multiplies that fixed \
                 cost by its trip count. Grouping the animations into one play \
                 preserves the visuals when they are meant to run together; when \
                 they are meant to run one after another, LaggedStart reproduces \
                 the stagger in a single play."
                    .to_owned(),
                evidence,
            ));
        }
        diagnostics
    }
}
