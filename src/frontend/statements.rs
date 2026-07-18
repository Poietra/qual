//! Statement-position and per-file binding facts (DESIGN §5.6: rules query
//! facts, they do not walk the AST).
//!
//! This module owns three things the rule layer used to hand-roll:
//!
//! 1. **The canonical AST traversal** ([`each_statement`],
//!    [`statement_exprs`], [`walk_expr`]): one depth-first statement walk,
//!    one statement→expression projection, one expression walk. Rules that
//!    still need to inspect a *fact-anchored* subtree (a resolved callback
//!    body, a class body located by a [`ClassRecord`] range) reuse these
//!    instead of maintaining private copies that can drift apart.
//! 2. **[`StatementFacts`]**: for every call expression of every parsed
//!    file, the byte span of its innermost enclosing statement and the
//!    call's [`StatementRole`] in that statement (bare expression statement,
//!    `with`-item context expression, assignment RHS, return value,
//!    decorator, or other). Computed once per analysis.
//! 3. **[`BindingFacts`]**: per-file import-derived name bindings with
//!    rebind poisoning (DESIGN §5.3: a name rebound anywhere in the file is
//!    never trusted), resolving local names and dotted chains to canonical
//!    dotted paths (`np.random.seed` → `numpy.random.seed`, an aliased
//!    `from math import inf as INF` → `math.inf`).
//!
//! The binding facts are deliberately *file-flat and flow-insensitive*: they
//! answer "can this name still be trusted to mean its import target
//! anywhere in this file?", which is the conservative contract the
//! determinism rules need. Scope-correct, flow-sensitive resolution is the
//! job of [`super::index`]; the two layers answer different questions and
//! rules combine them (candidates first, file-conservative fallback second).
//!
//! [`ClassRecord`]: super::index::ClassRecord

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::ast::{self, Ranged};
use rustpython_parser::text_size::TextRange;

use super::index::QualifiedCall;
use super::names::{collect_pattern_names, collect_target_names, flatten_dotted};
use super::parser::parsed_modules;
use crate::source::{FileId, SourceFile, SourceManager};

// ---------------------------------------------------------------------------
// Canonical traversal.
// ---------------------------------------------------------------------------

/// Depth-first visit over every statement of a statement list, entering all
/// compound-statement bodies (function / class bodies, branches, loops,
/// `with`, `try` handlers, `match` cases).
pub fn each_statement<'a>(stmts: &'a [ast::Stmt], visit: &mut dyn FnMut(&'a ast::Stmt)) {
    for stmt in stmts {
        visit(stmt);
        match stmt {
            ast::Stmt::FunctionDef(inner) => each_statement(&inner.body, visit),
            ast::Stmt::AsyncFunctionDef(inner) => each_statement(&inner.body, visit),
            ast::Stmt::ClassDef(inner) => each_statement(&inner.body, visit),
            ast::Stmt::If(inner) => {
                each_statement(&inner.body, visit);
                each_statement(&inner.orelse, visit);
            }
            ast::Stmt::While(inner) => {
                each_statement(&inner.body, visit);
                each_statement(&inner.orelse, visit);
            }
            ast::Stmt::For(inner) => {
                each_statement(&inner.body, visit);
                each_statement(&inner.orelse, visit);
            }
            ast::Stmt::AsyncFor(inner) => {
                each_statement(&inner.body, visit);
                each_statement(&inner.orelse, visit);
            }
            ast::Stmt::With(inner) => each_statement(&inner.body, visit),
            ast::Stmt::AsyncWith(inner) => each_statement(&inner.body, visit),
            ast::Stmt::Try(inner) => {
                each_statement(&inner.body, visit);
                for handler in &inner.handlers {
                    let ast::ExceptHandler::ExceptHandler(handler) = handler;
                    each_statement(&handler.body, visit);
                }
                each_statement(&inner.orelse, visit);
                each_statement(&inner.finalbody, visit);
            }
            ast::Stmt::TryStar(inner) => {
                each_statement(&inner.body, visit);
                for handler in &inner.handlers {
                    let ast::ExceptHandler::ExceptHandler(handler) = handler;
                    each_statement(&handler.body, visit);
                }
                each_statement(&inner.orelse, visit);
                each_statement(&inner.finalbody, visit);
            }
            ast::Stmt::Match(inner) => {
                for case in &inner.cases {
                    each_statement(&case.body, visit);
                }
            }
            _ => {}
        }
    }
}

/// Depth-first statement walk over one class body's *instance scope*:
/// enters direct methods and their nested functions but never a nested
/// `class` body, whose `self` is a different instance. The visitor
/// receives each statement together with the function nesting depth of
/// its enclosing context (`0` = directly in the class body, `1` = inside
/// a method, ...). A nested `class` statement is itself visited — its
/// decorators and bases evaluate in the outer scope — but its body is
/// skipped entirely.
pub fn each_class_scope_statement<'a>(
    body: &'a [ast::Stmt],
    visit: &mut dyn FnMut(&'a ast::Stmt, u32),
) {
    fn walk<'a>(stmts: &'a [ast::Stmt], depth: u32, visit: &mut dyn FnMut(&'a ast::Stmt, u32)) {
        for stmt in stmts {
            visit(stmt, depth);
            match stmt {
                ast::Stmt::FunctionDef(inner) => walk(&inner.body, depth + 1, visit),
                ast::Stmt::AsyncFunctionDef(inner) => walk(&inner.body, depth + 1, visit),
                // A nested class owns a different `self`: never descend.
                #[allow(clippy::match_same_arms, reason = "deliberate: never descend")]
                ast::Stmt::ClassDef(_) => {}
                ast::Stmt::If(inner) => {
                    walk(&inner.body, depth, visit);
                    walk(&inner.orelse, depth, visit);
                }
                ast::Stmt::While(inner) => {
                    walk(&inner.body, depth, visit);
                    walk(&inner.orelse, depth, visit);
                }
                ast::Stmt::For(inner) => {
                    walk(&inner.body, depth, visit);
                    walk(&inner.orelse, depth, visit);
                }
                ast::Stmt::AsyncFor(inner) => {
                    walk(&inner.body, depth, visit);
                    walk(&inner.orelse, depth, visit);
                }
                ast::Stmt::With(inner) => walk(&inner.body, depth, visit),
                ast::Stmt::AsyncWith(inner) => walk(&inner.body, depth, visit),
                ast::Stmt::Try(inner) => {
                    walk(&inner.body, depth, visit);
                    for handler in &inner.handlers {
                        let ast::ExceptHandler::ExceptHandler(handler) = handler;
                        walk(&handler.body, depth, visit);
                    }
                    walk(&inner.orelse, depth, visit);
                    walk(&inner.finalbody, depth, visit);
                }
                ast::Stmt::TryStar(inner) => {
                    walk(&inner.body, depth, visit);
                    for handler in &inner.handlers {
                        let ast::ExceptHandler::ExceptHandler(handler) = handler;
                        walk(&handler.body, depth, visit);
                    }
                    walk(&inner.orelse, depth, visit);
                    walk(&inner.finalbody, depth, visit);
                }
                ast::Stmt::Match(inner) => {
                    for case in &inner.cases {
                        walk(&case.body, depth, visit);
                    }
                }
                _ => {}
            }
        }
    }
    walk(body, 0, visit);
}

/// The expressions embedded directly in one statement. Compound bodies are
/// reached by [`each_statement`], not here, so a nested statement's
/// expressions are attributed to the nested statement.
#[must_use]
#[allow(clippy::too_many_lines, reason = "one arm per statement kind")]
pub fn statement_exprs(stmt: &ast::Stmt) -> Vec<&ast::Expr> {
    let mut exprs: Vec<&ast::Expr> = Vec::new();
    match stmt {
        ast::Stmt::Assign(inner) => {
            exprs.extend(inner.targets.iter());
            exprs.push(&inner.value);
        }
        ast::Stmt::AnnAssign(inner) => {
            exprs.push(&inner.target);
            if let Some(value) = &inner.value {
                exprs.push(value);
            }
        }
        ast::Stmt::AugAssign(inner) => {
            exprs.push(&inner.target);
            exprs.push(&inner.value);
        }
        ast::Stmt::Expr(inner) => exprs.push(&inner.value),
        ast::Stmt::Return(inner) => {
            if let Some(value) = &inner.value {
                exprs.push(value);
            }
        }
        ast::Stmt::Delete(inner) => exprs.extend(inner.targets.iter()),
        ast::Stmt::For(inner) => {
            exprs.push(&inner.target);
            exprs.push(&inner.iter);
        }
        ast::Stmt::AsyncFor(inner) => {
            exprs.push(&inner.target);
            exprs.push(&inner.iter);
        }
        ast::Stmt::While(inner) => exprs.push(&inner.test),
        ast::Stmt::If(inner) => exprs.push(&inner.test),
        ast::Stmt::With(inner) => {
            for item in &inner.items {
                exprs.push(&item.context_expr);
                if let Some(vars) = &item.optional_vars {
                    exprs.push(vars);
                }
            }
        }
        ast::Stmt::AsyncWith(inner) => {
            for item in &inner.items {
                exprs.push(&item.context_expr);
                if let Some(vars) = &item.optional_vars {
                    exprs.push(vars);
                }
            }
        }
        ast::Stmt::Raise(inner) => {
            if let Some(exc) = &inner.exc {
                exprs.push(exc);
            }
            if let Some(cause) = &inner.cause {
                exprs.push(cause);
            }
        }
        ast::Stmt::Assert(inner) => {
            exprs.push(&inner.test);
            if let Some(message) = &inner.msg {
                exprs.push(message);
            }
        }
        ast::Stmt::Match(inner) => {
            exprs.push(&inner.subject);
            for case in &inner.cases {
                if let Some(guard) = &case.guard {
                    exprs.push(guard);
                }
            }
        }
        ast::Stmt::FunctionDef(inner) => {
            exprs.extend(inner.decorator_list.iter());
            exprs.extend(default_exprs(&inner.args));
        }
        ast::Stmt::AsyncFunctionDef(inner) => {
            exprs.extend(inner.decorator_list.iter());
            exprs.extend(default_exprs(&inner.args));
        }
        ast::Stmt::ClassDef(inner) => {
            exprs.extend(inner.decorator_list.iter());
            exprs.extend(inner.bases.iter());
            for keyword in &inner.keywords {
                exprs.push(&keyword.value);
            }
        }
        _ => {}
    }
    exprs
}

fn default_exprs(args: &ast::Arguments) -> Vec<&ast::Expr> {
    let mut exprs: Vec<&ast::Expr> = Vec::new();
    for arg in args
        .posonlyargs
        .iter()
        .chain(&args.args)
        .chain(&args.kwonlyargs)
    {
        if let Some(default) = &arg.default {
            exprs.push(default);
        }
    }
    exprs
}

/// Depth-first visit over an expression and all of its subexpressions
/// (lambda parameter defaults and bodies, comprehension parts, f-string
/// format specs included).
#[allow(clippy::too_many_lines, reason = "one arm per expression kind")]
pub fn walk_expr<'a>(expr: &'a ast::Expr, visit: &mut dyn FnMut(&'a ast::Expr)) {
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
        ast::Expr::Lambda(inner) => {
            for default in default_exprs(&inner.args) {
                walk_expr(default, visit);
            }
            walk_expr(&inner.body, visit);
        }
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
        ast::Expr::FormattedValue(inner) => {
            walk_expr(&inner.value, visit);
            if let Some(spec) = &inner.format_spec {
                walk_expr(spec, visit);
            }
        }
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

/// Certainty-annotated expression walk: visits `expr` and exactly the
/// subexpressions that are certainly evaluated whenever `expr` is. Lambda
/// bodies (deferred), comprehension parts (zero-iteration), conditional
/// expression branches, and short-circuit tails are skipped entirely, like
/// leaves. Conservative by construction — anything skipped only means a
/// rule stays silent (DESIGN §15: no certainty from `Maybe`).
#[allow(clippy::too_many_lines, reason = "one arm per expression kind")]
pub fn walk_certain_exprs<'a>(expr: &'a ast::Expr, visit: &mut dyn FnMut(&'a ast::Expr)) {
    visit(expr);
    match expr {
        // Only the first operand of a short-circuit chain is certain.
        ast::Expr::BoolOp(inner) => {
            if let Some(first) = inner.values.first() {
                walk_certain_exprs(first, visit);
            }
        }
        // Only the test of a conditional expression is certain.
        ast::Expr::IfExp(inner) => walk_certain_exprs(&inner.test, visit),
        // Lambda bodies are deferred; comprehension parts may run zero
        // times. Both subtrees are skipped entirely, like leaves.
        ast::Expr::Lambda(_)
        | ast::Expr::ListComp(_)
        | ast::Expr::SetComp(_)
        | ast::Expr::DictComp(_)
        | ast::Expr::GeneratorExp(_)
        | ast::Expr::Constant(_)
        | ast::Expr::Name(_) => {}
        ast::Expr::NamedExpr(inner) => {
            walk_certain_exprs(&inner.target, visit);
            walk_certain_exprs(&inner.value, visit);
        }
        ast::Expr::BinOp(inner) => {
            walk_certain_exprs(&inner.left, visit);
            walk_certain_exprs(&inner.right, visit);
        }
        ast::Expr::UnaryOp(inner) => walk_certain_exprs(&inner.operand, visit),
        ast::Expr::Dict(inner) => {
            for key in inner.keys.iter().flatten() {
                walk_certain_exprs(key, visit);
            }
            for value in &inner.values {
                walk_certain_exprs(value, visit);
            }
        }
        ast::Expr::Set(inner) => {
            for element in &inner.elts {
                walk_certain_exprs(element, visit);
            }
        }
        ast::Expr::Await(inner) => walk_certain_exprs(&inner.value, visit),
        ast::Expr::Yield(inner) => {
            if let Some(value) = &inner.value {
                walk_certain_exprs(value, visit);
            }
        }
        ast::Expr::YieldFrom(inner) => walk_certain_exprs(&inner.value, visit),
        ast::Expr::Compare(inner) => {
            walk_certain_exprs(&inner.left, visit);
            for comparator in &inner.comparators {
                walk_certain_exprs(comparator, visit);
            }
        }
        ast::Expr::Call(inner) => {
            walk_certain_exprs(&inner.func, visit);
            for argument in &inner.args {
                walk_certain_exprs(argument, visit);
            }
            for keyword in &inner.keywords {
                walk_certain_exprs(&keyword.value, visit);
            }
        }
        ast::Expr::FormattedValue(inner) => {
            walk_certain_exprs(&inner.value, visit);
            if let Some(spec) = &inner.format_spec {
                walk_certain_exprs(spec, visit);
            }
        }
        ast::Expr::JoinedStr(inner) => {
            for value in &inner.values {
                walk_certain_exprs(value, visit);
            }
        }
        ast::Expr::Attribute(inner) => walk_certain_exprs(&inner.value, visit),
        ast::Expr::Subscript(inner) => {
            walk_certain_exprs(&inner.value, visit);
            walk_certain_exprs(&inner.slice, visit);
        }
        ast::Expr::Starred(inner) => walk_certain_exprs(&inner.value, visit),
        ast::Expr::List(inner) => {
            for element in &inner.elts {
                walk_certain_exprs(element, visit);
            }
        }
        ast::Expr::Tuple(inner) => {
            for element in &inner.elts {
                walk_certain_exprs(element, visit);
            }
        }
        ast::Expr::Slice(inner) => {
            for part in [&inner.lower, &inner.upper, &inner.step]
                .into_iter()
                .flatten()
            {
                walk_certain_exprs(part, visit);
            }
        }
    }
}

/// Visits every expression of a parsed file: each statement's direct
/// expressions plus all nested subexpressions.
fn each_expr_in_file<'a>(file: &'a SourceFile, visit: &mut dyn FnMut(&'a ast::Expr)) {
    let Some(module) = file.ast() else {
        return;
    };
    each_statement(&module.body, &mut |stmt| {
        for expr in statement_exprs(stmt) {
            walk_expr(expr, visit);
        }
    });
}

// ---------------------------------------------------------------------------
// AST node queries (fact-anchored navigation).
//
// Rules hold byte ranges from facts (a `ClassRecord`, a lambda argument's
// span) and need the corresponding node to inspect a *local* subtree. The
// lookups live here so the module-root walk stays a frontend concern.
// ---------------------------------------------------------------------------

/// The `def` statement spanning exactly `range` in `file`, if any.
#[must_use]
pub fn function_def_at(file: &SourceFile, range: TextRange) -> Option<&ast::StmtFunctionDef> {
    let module = file.ast()?;
    let mut found: Option<&ast::StmtFunctionDef> = None;
    each_statement(&module.body, &mut |stmt| {
        if let ast::Stmt::FunctionDef(def) = stmt {
            if def.range() == range {
                found = Some(def);
            }
        }
    });
    found
}

/// The `class` statement spanning exactly `range` in `file`, if any.
#[must_use]
pub fn class_def_at(file: &SourceFile, range: TextRange) -> Option<&ast::StmtClassDef> {
    let module = file.ast()?;
    let mut found: Option<&ast::StmtClassDef> = None;
    each_statement(&module.body, &mut |stmt| {
        if let ast::Stmt::ClassDef(def) = stmt {
            if def.range() == range {
                found = Some(def);
            }
        }
    });
    found
}

/// The single `def <name>` in the whole file (nested defs included), or
/// `None` when the name is absent or ambiguous — resolution never guesses.
#[must_use]
pub fn unique_function_def<'a>(
    file: &'a SourceFile,
    name: &str,
) -> Option<&'a ast::StmtFunctionDef> {
    let module = file.ast()?;
    let mut matches: Vec<&ast::StmtFunctionDef> = Vec::new();
    each_statement(&module.body, &mut |stmt| {
        if let ast::Stmt::FunctionDef(def) = stmt {
            if def.name.as_str() == name {
                matches.push(def);
            }
        }
    });
    match matches.as_slice() {
        [def] => Some(def),
        _ => None,
    }
}

/// The `for` statement whose *direct* loop body contains a statement
/// spanning exactly `statement` in `file`, if any. Statement byte ranges
/// are unique (a compound statement strictly contains its children), so
/// at most one loop matches. `orelse` suites deliberately do not count:
/// they run once, not per iteration.
#[must_use]
pub fn for_loop_with_body_statement(
    file: &SourceFile,
    statement: TextRange,
) -> Option<&ast::StmtFor> {
    let module = file.ast()?;
    let mut found: Option<&ast::StmtFor> = None;
    each_statement(&module.body, &mut |stmt| {
        if let ast::Stmt::For(for_stmt) = stmt {
            if for_stmt.body.iter().any(|child| child.range() == statement) {
                found = Some(for_stmt);
            }
        }
    });
    found
}

/// The lambda expression spanning exactly `range` in `file`, if any.
#[must_use]
pub fn lambda_at(file: &SourceFile, range: TextRange) -> Option<&ast::ExprLambda> {
    let mut found: Option<&ast::ExprLambda> = None;
    each_expr_in_file(file, &mut |expr| {
        if let ast::Expr::Lambda(lambda) = expr {
            if lambda.range() == range {
                found = Some(lambda);
            }
        }
    });
    found
}

// ---------------------------------------------------------------------------
// Statement facts.
// ---------------------------------------------------------------------------

/// The syntactic position of a call expression inside its innermost
/// enclosing statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatementRole {
    /// The call is the whole value of an expression statement (`f(...)` on
    /// its own, not assigned, not passed on).
    BareExpression,
    /// The call is a `with`-item context expression (`with f(...):` /
    /// `with f(...) as x:`).
    WithContext,
    /// The call is the whole right-hand side of a plain assignment.
    /// `target` carries the bound name when the assignment has exactly one
    /// plain-name target (`x = f(...)`), `None` otherwise (tuple targets,
    /// attribute targets, chained assignments).
    AssignmentRhs {
        /// The single plain-name target, if the assignment has exactly one.
        target: Option<String>,
    },
    /// The call is the whole `return` value.
    ReturnValue,
    /// The call is a decorator expression of a `def` / `class` statement.
    Decorator,
    /// Any other position (an argument, an operand, a nested expression,
    /// a compound statement's test, ...).
    Other,
}

/// Statement-position facts of one call expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallStatementFact {
    /// Byte span of the innermost statement containing the call. For a
    /// decorator call this is the whole decorated `def` / `class` statement.
    pub statement_range: TextRange,
    /// The call's syntactic role in that statement.
    pub role: StatementRole,
}

/// Statement-position facts for every call expression of every parsed
/// file, keyed by the call's exact byte range.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatementFacts {
    by_call: BTreeMap<(FileId, u32, u32), CallStatementFact>,
}

impl StatementFacts {
    /// Collects the facts over all parsed files of `sources`.
    #[must_use]
    pub fn collect(sources: &SourceManager) -> Self {
        let mut by_call = BTreeMap::new();
        for module in parsed_modules(sources) {
            collect_statement_facts(module.file, &module.ast.body, &mut by_call);
        }
        Self { by_call }
    }

    /// The fact for the call expression spanning exactly `range` in `file`.
    #[must_use]
    pub fn at(&self, file: FileId, range: TextRange) -> Option<&CallStatementFact> {
        self.by_call
            .get(&(file, range.start().into(), range.end().into()))
    }

    /// The fact for a qualified-call fact's whole call expression.
    #[must_use]
    pub fn for_call(&self, call: &QualifiedCall) -> Option<&CallStatementFact> {
        self.at(call.file, call.call_range)
    }
}

fn collect_statement_facts(
    file: FileId,
    body: &[ast::Stmt],
    by_call: &mut BTreeMap<(FileId, u32, u32), CallStatementFact>,
) {
    each_statement(body, &mut |stmt| {
        let statement_range = stmt.range();
        let mut record = |range: TextRange, role: StatementRole| {
            by_call.insert(
                (file, range.start().into(), range.end().into()),
                CallStatementFact {
                    statement_range,
                    role,
                },
            );
        };
        // Every call directly owned by this statement defaults to `Other`;
        // the role anchors below override the exact-match spans. Nested
        // statements record their own calls, so the innermost statement
        // always wins.
        for expr in statement_exprs(stmt) {
            walk_expr(expr, &mut |candidate| {
                if let ast::Expr::Call(call) = candidate {
                    record(call.range(), StatementRole::Other);
                }
            });
        }
        let mut anchor = |expr: &ast::Expr, role: StatementRole| {
            if let ast::Expr::Call(call) = expr {
                record(call.range(), role);
            }
        };
        match stmt {
            ast::Stmt::Expr(inner) => anchor(&inner.value, StatementRole::BareExpression),
            ast::Stmt::Assign(inner) => {
                let target = match inner.targets.as_slice() {
                    [ast::Expr::Name(name)] => Some(name.id.to_string()),
                    _ => None,
                };
                anchor(&inner.value, StatementRole::AssignmentRhs { target });
            }
            ast::Stmt::Return(inner) => {
                if let Some(value) = &inner.value {
                    anchor(value, StatementRole::ReturnValue);
                }
            }
            ast::Stmt::With(inner) => {
                for item in &inner.items {
                    anchor(&item.context_expr, StatementRole::WithContext);
                }
            }
            ast::Stmt::AsyncWith(inner) => {
                for item in &inner.items {
                    anchor(&item.context_expr, StatementRole::WithContext);
                }
            }
            ast::Stmt::FunctionDef(inner) => {
                for decorator in &inner.decorator_list {
                    anchor(decorator, StatementRole::Decorator);
                }
            }
            ast::Stmt::AsyncFunctionDef(inner) => {
                for decorator in &inner.decorator_list {
                    anchor(decorator, StatementRole::Decorator);
                }
            }
            ast::Stmt::ClassDef(inner) => {
                for decorator in &inner.decorator_list {
                    anchor(decorator, StatementRole::Decorator);
                }
            }
            _ => {}
        }
    });
}

// ---------------------------------------------------------------------------
// Binding facts.
// ---------------------------------------------------------------------------

/// Conservative import-derived name facts of one file (DESIGN §5.3).
///
/// A name appears in the import maps only when it is bound by exactly one
/// absolute import shape and by nothing else anywhere in the file.
/// Everything that binds a name outside a resolvable import — assignments,
/// augmented / annotated assignments, `for` / `with` targets, function and
/// class definitions, parameters (defs and lambdas), comprehension targets,
/// walrus targets, `except ... as`, `match` captures, `global` /
/// `nonlocal`, and relative / unresolvable imports — poisons it: a
/// poisoned name is never trusted to mean its import target.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileBindingFacts {
    /// Local name → imported module dotted path (`np` → `numpy`).
    modules: BTreeMap<String, String>,
    /// Local name → `module.member` path (`urlopen` →
    /// `urllib.request.urlopen`).
    members: BTreeMap<String, String>,
    /// Absolute modules star-imported into the file (`from manim import *`).
    star_modules: BTreeSet<String>,
    /// A star import whose source module could not be resolved (relative
    /// star) exists somewhere in the file.
    has_unresolved_star: bool,
    /// Names bound by anything that is not a resolvable absolute import.
    poisoned: BTreeSet<String>,
    /// Names bound as targets of assignment-like statements (`=` / `:=` /
    /// augmented / annotated assignments, `for` targets, `with ... as`).
    assigned: BTreeSet<String>,
    /// Number of statements binding each name (all binding kinds).
    binding_statements: BTreeMap<String, usize>,
}

impl FileBindingFacts {
    /// Resolves a dotted chain to its canonical dotted path through the
    /// file's imports (`["np", "random", "seed"]` → `numpy.random.seed`).
    #[must_use]
    pub fn resolve_parts(&self, parts: &[String]) -> Option<String> {
        let (first, rest) = parts.split_first()?;
        let base = self
            .modules
            .get(first.as_str())
            .or_else(|| self.members.get(first.as_str()))?;
        if rest.is_empty() {
            return Some(base.clone());
        }
        Some(format!("{base}.{}", rest.join(".")))
    }

    /// Resolves a pure `Name` / `Attribute` chain expression to its
    /// canonical dotted path. `None` for dynamic expressions (calls,
    /// subscripts, ...) — resolution never guesses.
    #[must_use]
    pub fn resolve_expr(&self, expr: &ast::Expr) -> Option<String> {
        let (root, segments) = flatten_dotted(expr)?;
        let mut parts = vec![root];
        parts.extend(segments);
        self.resolve_parts(&parts)
    }

    /// The import target bound to `name`, when exactly one conflict-free
    /// absolute import binds it and nothing poisons it.
    #[must_use]
    pub fn import_target(&self, name: &str) -> Option<&str> {
        self.members
            .get(name)
            .or_else(|| self.modules.get(name))
            .map(String::as_str)
    }

    /// Whether `name` is bound by anything that is not a resolvable
    /// absolute import.
    #[must_use]
    pub fn is_poisoned(&self, name: &str) -> bool {
        self.poisoned.contains(name)
    }

    /// Whether `name` is a completely unshadowed bare name in this file:
    /// no import binds it and nothing poisons it. For a builtin name this
    /// means "can only be the builtin" (star imports are judged
    /// separately via [`FileBindingFacts::has_star_from`]).
    #[must_use]
    pub fn is_unshadowed_bare(&self, name: &str) -> bool {
        !self.poisoned.contains(name)
            && !self.modules.contains_key(name)
            && !self.members.contains_key(name)
    }

    /// Whether the file star-imports `module` (`from module import *`).
    #[must_use]
    pub fn has_star_from(&self, module: &str) -> bool {
        self.star_modules.contains(module)
    }

    /// Whether any star import other than `from module import *` exists
    /// (unresolvable relative stars included).
    #[must_use]
    pub fn has_star_other_than(&self, module: &str) -> bool {
        self.has_unresolved_star || self.star_modules.iter().any(|source| source != module)
    }

    /// Whether `name` is bound as a target of an assignment-like statement
    /// or walrus expression anywhere in the file.
    #[must_use]
    pub fn is_statement_assigned(&self, name: &str) -> bool {
        self.assigned.contains(name)
    }

    /// The number of statements that bind `name` anywhere in the file
    /// (all binding kinds: assignments, defs, classes, imports, targets,
    /// parameters, expression binders, `global` / `nonlocal`).
    #[must_use]
    pub fn binding_statement_count(&self, name: &str) -> usize {
        self.binding_statements.get(name).copied().unwrap_or(0)
    }

    #[allow(clippy::too_many_lines, reason = "one arm per binding statement kind")]
    fn build(module: &ast::ModModule) -> Self {
        let mut facts = Self::default();
        let mut conflicting: BTreeSet<String> = BTreeSet::new();
        each_statement(&module.body, &mut |stmt| {
            // Names this one statement binds, for the per-name statement
            // counts (a statement binding a name twice counts once).
            let mut bound_here: BTreeSet<String> = BTreeSet::new();
            match stmt {
                ast::Stmt::Import(import) => {
                    for alias in &import.names {
                        let (bound, target) = alias.asname.as_ref().map_or_else(
                            || {
                                let top = alias
                                    .name
                                    .split('.')
                                    .next()
                                    .unwrap_or(alias.name.as_str())
                                    .to_owned();
                                (top.clone(), top)
                            },
                            |asname| (asname.to_string(), alias.name.to_string()),
                        );
                        bound_here.insert(bound.clone());
                        record_binding(&mut facts.modules, &mut conflicting, bound, target);
                    }
                }
                ast::Stmt::ImportFrom(import) => {
                    let relative = import.level.is_some_and(|level| level.to_usize() > 0);
                    let source = if relative {
                        None
                    } else {
                        import.module.as_deref()
                    };
                    for alias in &import.names {
                        if alias.name.as_str() == "*" {
                            match source {
                                Some(module) => {
                                    facts.star_modules.insert(module.to_owned());
                                }
                                None => facts.has_unresolved_star = true,
                            }
                            continue;
                        }
                        let bound = alias.asname.as_ref().map_or_else(
                            || alias.name.to_string(),
                            std::string::ToString::to_string,
                        );
                        bound_here.insert(bound.clone());
                        match source {
                            Some(module) => {
                                let target = format!("{module}.{}", alias.name);
                                record_binding(&mut facts.members, &mut conflicting, bound, target);
                            }
                            // Relative / unresolvable import shapes are
                            // never trusted: poison the bound name.
                            None => {
                                facts.poisoned.insert(bound);
                            }
                        }
                    }
                }
                ast::Stmt::Assign(assign) => {
                    for target in &assign.targets {
                        collect_target_names(target, &mut bound_here);
                    }
                    facts.poisoned.extend(bound_here.iter().cloned());
                    facts.assigned.extend(bound_here.iter().cloned());
                }
                ast::Stmt::AnnAssign(assign) => {
                    collect_target_names(&assign.target, &mut bound_here);
                    facts.poisoned.extend(bound_here.iter().cloned());
                    facts.assigned.extend(bound_here.iter().cloned());
                }
                ast::Stmt::AugAssign(assign) => {
                    collect_target_names(&assign.target, &mut bound_here);
                    facts.poisoned.extend(bound_here.iter().cloned());
                    facts.assigned.extend(bound_here.iter().cloned());
                }
                ast::Stmt::For(inner) => {
                    collect_target_names(&inner.target, &mut bound_here);
                    facts.poisoned.extend(bound_here.iter().cloned());
                    facts.assigned.extend(bound_here.iter().cloned());
                }
                ast::Stmt::AsyncFor(inner) => {
                    collect_target_names(&inner.target, &mut bound_here);
                    facts.poisoned.extend(bound_here.iter().cloned());
                    facts.assigned.extend(bound_here.iter().cloned());
                }
                ast::Stmt::With(inner) => {
                    for item in &inner.items {
                        if let Some(vars) = &item.optional_vars {
                            collect_target_names(vars, &mut bound_here);
                        }
                    }
                    facts.poisoned.extend(bound_here.iter().cloned());
                    facts.assigned.extend(bound_here.iter().cloned());
                }
                ast::Stmt::AsyncWith(inner) => {
                    for item in &inner.items {
                        if let Some(vars) = &item.optional_vars {
                            collect_target_names(vars, &mut bound_here);
                        }
                    }
                    facts.poisoned.extend(bound_here.iter().cloned());
                    facts.assigned.extend(bound_here.iter().cloned());
                }
                ast::Stmt::FunctionDef(def) => {
                    bound_here.insert(def.name.to_string());
                    collect_parameter_names(&def.args, &mut bound_here);
                    facts.poisoned.extend(bound_here.iter().cloned());
                }
                ast::Stmt::AsyncFunctionDef(def) => {
                    bound_here.insert(def.name.to_string());
                    collect_parameter_names(&def.args, &mut bound_here);
                    facts.poisoned.extend(bound_here.iter().cloned());
                }
                ast::Stmt::ClassDef(def) => {
                    bound_here.insert(def.name.to_string());
                    facts.poisoned.extend(bound_here.iter().cloned());
                }
                ast::Stmt::Try(inner) => {
                    for handler in &inner.handlers {
                        let ast::ExceptHandler::ExceptHandler(handler) = handler;
                        if let Some(name) = &handler.name {
                            bound_here.insert(name.to_string());
                        }
                    }
                    facts.poisoned.extend(bound_here.iter().cloned());
                }
                ast::Stmt::TryStar(inner) => {
                    for handler in &inner.handlers {
                        let ast::ExceptHandler::ExceptHandler(handler) = handler;
                        if let Some(name) = &handler.name {
                            bound_here.insert(name.to_string());
                        }
                    }
                    facts.poisoned.extend(bound_here.iter().cloned());
                }
                ast::Stmt::Match(inner) => {
                    for case in &inner.cases {
                        collect_pattern_names(&case.pattern, &mut bound_here);
                    }
                    facts.poisoned.extend(bound_here.iter().cloned());
                }
                ast::Stmt::Global(inner) => {
                    for name in &inner.names {
                        bound_here.insert(name.to_string());
                    }
                    facts.poisoned.extend(bound_here.iter().cloned());
                }
                ast::Stmt::Nonlocal(inner) => {
                    for name in &inner.names {
                        bound_here.insert(name.to_string());
                    }
                    facts.poisoned.extend(bound_here.iter().cloned());
                }
                _ => {}
            }
            // Lambda parameters, walrus targets, and comprehension targets
            // bind inside expressions.
            for expr in statement_exprs(stmt) {
                walk_expr(expr, &mut |inner| match inner {
                    ast::Expr::Lambda(lambda) => {
                        let mut names = BTreeSet::new();
                        collect_parameter_names(&lambda.args, &mut names);
                        facts.poisoned.extend(names.iter().cloned());
                        bound_here.extend(names);
                    }
                    ast::Expr::NamedExpr(walrus) => {
                        let mut names = BTreeSet::new();
                        collect_target_names(&walrus.target, &mut names);
                        facts.poisoned.extend(names.iter().cloned());
                        facts.assigned.extend(names.iter().cloned());
                        bound_here.extend(names);
                    }
                    ast::Expr::ListComp(comp) => {
                        comprehension_names(&comp.generators, &mut facts.poisoned, &mut bound_here);
                    }
                    ast::Expr::SetComp(comp) => {
                        comprehension_names(&comp.generators, &mut facts.poisoned, &mut bound_here);
                    }
                    ast::Expr::DictComp(comp) => {
                        comprehension_names(&comp.generators, &mut facts.poisoned, &mut bound_here);
                    }
                    ast::Expr::GeneratorExp(comp) => {
                        comprehension_names(&comp.generators, &mut facts.poisoned, &mut bound_here);
                    }
                    _ => {}
                });
            }
            for name in bound_here {
                *facts.binding_statements.entry(name).or_insert(0) += 1;
            }
        });
        // A name bound as both a module alias and a member is ambiguous,
        // and a poisoned name is never trusted.
        let both: Vec<String> = facts
            .modules
            .keys()
            .filter(|name| facts.members.contains_key(*name))
            .cloned()
            .collect();
        conflicting.extend(both);
        for name in conflicting.iter().chain(&facts.poisoned) {
            facts.modules.remove(name);
            facts.members.remove(name);
        }
        facts
    }
}

fn comprehension_names(
    generators: &[ast::Comprehension],
    poisoned: &mut BTreeSet<String>,
    bound_here: &mut BTreeSet<String>,
) {
    let mut names = BTreeSet::new();
    for generator in generators {
        collect_target_names(&generator.target, &mut names);
    }
    poisoned.extend(names.iter().cloned());
    bound_here.extend(names);
}

fn collect_parameter_names(args: &ast::Arguments, names: &mut BTreeSet<String>) {
    for arg in args
        .posonlyargs
        .iter()
        .chain(&args.args)
        .chain(&args.kwonlyargs)
    {
        names.insert(arg.def.arg.to_string());
    }
    if let Some(vararg) = &args.vararg {
        names.insert(vararg.arg.to_string());
    }
    if let Some(kwarg) = &args.kwarg {
        names.insert(kwarg.arg.to_string());
    }
}

fn record_binding(
    map: &mut BTreeMap<String, String>,
    conflicting: &mut BTreeSet<String>,
    bound: String,
    target: String,
) {
    match map.get(&bound) {
        Some(existing) if *existing != target => {
            conflicting.insert(bound);
        }
        Some(_) => {}
        None => {
            map.insert(bound, target);
        }
    }
}

/// Per-file binding facts for every parsed file of the project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BindingFacts {
    files: BTreeMap<FileId, FileBindingFacts>,
}

impl BindingFacts {
    /// Collects the facts over all parsed files of `sources`.
    #[must_use]
    pub fn collect(sources: &SourceManager) -> Self {
        let mut files = BTreeMap::new();
        for module in parsed_modules(sources) {
            files.insert(module.file, FileBindingFacts::build(module.ast));
        }
        Self { files }
    }

    /// The binding facts of one file. `None` when the file was not parsed.
    #[must_use]
    pub fn file(&self, file: FileId) -> Option<&FileBindingFacts> {
        self.files.get(&file)
    }
}
