//! Determinism / portability / cache-stability rules (`MLD3xx`, DESIGN §7.4).
//!
//! Implemented: `MLD301` (FPS-dependent updater motion), `MLD302` (unseeded
//! global random state in frame callbacks), `MLD303` (foreign-platform
//! absolute asset paths), `MLD304` (renderer-divergent semantics without a
//! guard under a multi-renderer run), `MLD305` (case-only asset path
//! mismatches on case-sensitive target platforms), `MLD306` (fonts outside
//! the profile allowlist), and `MLD307` (wall-clock / filesystem / network
//! calls inside frame callbacks).
//!
//! Every rule fires only on confirmed facts: knowledge-resolved call
//! candidates, literal argument facts, proven hot contexts, and definite
//! (all-paths) lifecycle events. Unknown facts always mean silence
//! (DESIGN §15 invariant 2). Name resolution through imports is
//! conservative: a name rebound anywhere in the file is never trusted.
//!
//! `build_diagnostic`, `single_knowledge_symbol`, `short_name`,
//! `each_statement`, and the case-insensitive path scan are shared with
//! `rules::rendering` (the owning module exports them `pub(crate)`).

mod fonts;
mod io_hooks;
mod paths;
mod randomness;
mod renderer_divergence;
mod updater_motion;

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::ast::{self, Ranged};

use crate::rules::base::Rule;
use crate::source::SourceFile;

/// Every implemented portability rule, in rule-ID order.
///
/// The registry composes this with the other rule-group modules; adding a
/// rule here is the only registration step a portability rule needs.
#[must_use]
pub fn rules() -> Vec<Box<dyn Rule>> {
    vec![
        Box::new(updater_motion::FpsDependentUpdaterMotion),
        Box::new(randomness::UnseededGlobalRandom),
        Box::new(paths::ForeignPlatformAssetPath),
        Box::new(renderer_divergence::UnguardedRendererDivergence),
        Box::new(paths::CaseOnlyAssetMismatch),
        Box::new(fonts::FontOutsideAllowlist),
        Box::new(io_hooks::FrameCallbackIo),
    ]
}

// ---------------------------------------------------------------------------
// Shared fact helpers (owned and exported by `rules::rendering`).
// ---------------------------------------------------------------------------

use crate::rules::rendering::{
    build_diagnostic, each_statement, short_name, single_knowledge_symbol,
};

// ---------------------------------------------------------------------------
// AST walking utilities.
// ---------------------------------------------------------------------------

/// The expressions embedded directly in one statement (compound bodies are
/// reached by [`each_statement`], not here).
#[allow(clippy::too_many_lines, reason = "one arm per statement kind")]
fn statement_exprs(stmt: &ast::Stmt) -> Vec<&ast::Expr> {
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
/// (lambda bodies and comprehension parts included).
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

/// Visits every expression of a parsed file (statement expressions plus all
/// nested subexpressions, lambda bodies included).
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

/// Every `Call` expression of a file, keyed by its exact byte range. Used
/// to anchor rule logic on the AST node of a `QualifiedCall` fact.
fn call_nodes_by_range(file: &SourceFile) -> BTreeMap<(u32, u32), &ast::ExprCall> {
    let mut calls: BTreeMap<(u32, u32), &ast::ExprCall> = BTreeMap::new();
    each_expr_in_file(file, &mut |expr| {
        if let ast::Expr::Call(call) = expr {
            let range = call.range();
            calls.insert((range.start().into(), range.end().into()), call);
        }
    });
    calls
}

/// The dotted name of a pure `Name` / `Attribute` chain (`np.random.seed`
/// → `["np", "random", "seed"]`). `None` for anything else (calls,
/// subscripts, ...) — resolution never guesses.
fn dotted_parts(expr: &ast::Expr) -> Option<Vec<String>> {
    match expr {
        ast::Expr::Name(name) => Some(vec![name.id.to_string()]),
        ast::Expr::Attribute(attribute) => {
            let mut parts = dotted_parts(&attribute.value)?;
            parts.push(attribute.attr.to_string());
            Some(parts)
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Conservative per-file import resolution (DESIGN §5.3: rebinding anywhere
// means the name is never trusted).
// ---------------------------------------------------------------------------

/// Module-level import facts of one file.
///
/// A name is only present when it is bound by exactly one import shape and
/// by nothing else anywhere in the file (assignments, defs, parameters,
/// loop targets, patterns, lambda parameters, ... all poison it).
#[derive(Debug, Default)]
struct ImportMap {
    /// Local name → imported module dotted path (`np` → `numpy`).
    modules: BTreeMap<String, String>,
    /// Local name → `module.member` path (`urlopen` →
    /// `urllib.request.urlopen`).
    members: BTreeMap<String, String>,
    /// Modules star-imported into the file (`from manim import *`).
    star_modules: BTreeSet<String>,
    /// Names bound by anything that is not an import.
    poisoned: BTreeSet<String>,
}

impl ImportMap {
    /// Builds the map for one file.
    fn build(file: &SourceFile) -> Self {
        let mut map = Self {
            poisoned: non_import_bound_names(file),
            ..Self::default()
        };
        let Some(module) = file.ast() else {
            return map;
        };
        let mut conflicting: BTreeSet<String> = BTreeSet::new();
        each_statement(&module.body, &mut |stmt| match stmt {
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
                    record_binding(&mut map.modules, &mut conflicting, bound, target);
                }
            }
            ast::Stmt::ImportFrom(import) => {
                let relative = import.level.is_some_and(|level| level.to_usize() > 0);
                let Some(from_module) = import.module.as_deref() else {
                    return;
                };
                if relative {
                    return;
                }
                for alias in &import.names {
                    if alias.name.as_str() == "*" {
                        map.star_modules.insert(from_module.to_owned());
                        continue;
                    }
                    let bound = alias
                        .asname
                        .as_ref()
                        .map_or_else(|| alias.name.to_string(), std::string::ToString::to_string);
                    let target = format!("{from_module}.{}", alias.name);
                    record_binding(&mut map.members, &mut conflicting, bound, target);
                }
            }
            _ => {}
        });
        // A name bound as both a module alias and a member is ambiguous.
        let both: Vec<String> = map
            .modules
            .keys()
            .filter(|name| map.members.contains_key(*name))
            .cloned()
            .collect();
        conflicting.extend(both);
        for name in conflicting.iter().chain(&map.poisoned) {
            map.modules.remove(name);
            map.members.remove(name);
        }
        map
    }

    /// Resolves a dotted chain to its canonical dotted path through the
    /// file's imports (`["np", "random", "seed"]` → `numpy.random.seed`).
    fn resolve_parts(&self, parts: &[String]) -> Option<String> {
        let (first, rest) = parts.split_first()?;
        let base = self
            .modules
            .get(first)
            .or_else(|| self.members.get(first))?;
        if rest.is_empty() {
            return Some(base.clone());
        }
        Some(format!("{base}.{}", rest.join(".")))
    }

    /// Resolves the callee of a call to its canonical dotted path.
    fn resolve_expr(&self, expr: &ast::Expr) -> Option<String> {
        self.resolve_parts(&dotted_parts(expr)?)
    }

    /// Whether `name` is a completely unbound bare name in this file (no
    /// import, no other binding) — i.e. it can only be a builtin.
    fn is_probably_builtin(&self, name: &str) -> bool {
        !self.poisoned.contains(name)
            && !self.modules.contains_key(name)
            && !self.members.contains_key(name)
    }

    /// Whether `name` reaches a `from manim import *` binding untouched.
    fn is_manim_star_name(&self, name: &str) -> bool {
        self.star_modules.contains("manim")
            && !self.poisoned.contains(name)
            && !self.modules.contains_key(name)
            && !self.members.contains_key(name)
    }
}

/// Canonical dotted targets of a call to a (potential) external-library
/// function.
///
/// The frontend resolves imports scope-correctly and records external
/// third-party callees as their dotted path (`import time` + `time.time()`
/// → candidate `time.time`), so candidates are preferred when present —
/// but only when no candidate can name a *project* symbol (a project
/// module `time` would produce the same string; never guess). Without
/// candidates the file-level [`ImportMap`] resolution of the callee
/// expression is used.
fn external_call_targets(
    index: &crate::frontend::index::ProjectIndex,
    call: &crate::frontend::index::QualifiedCall,
    imports: &ImportMap,
    callee: &ast::Expr,
) -> Vec<String> {
    if call.candidates.is_empty() {
        return imports.resolve_expr(callee).into_iter().collect();
    }
    let all_external = call.candidates.iter().all(|candidate| {
        let top = candidate.split('.').next().unwrap_or(candidate);
        !index.module_tree.contains(top)
    });
    if all_external {
        call.candidates.iter().cloned().collect()
    } else {
        Vec::new()
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

/// Names bound anywhere in a file by anything that is not an import
/// statement: assignments, augmented / annotated assignments, `for` /
/// `with` targets, function and class definitions, parameters (defs and
/// lambdas), comprehension targets, walrus targets, `except ... as`,
/// `match` patterns, and `global` / `nonlocal` declarations.
#[allow(clippy::too_many_lines, reason = "one arm per binding statement kind")]
fn non_import_bound_names(file: &SourceFile) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let Some(module) = file.ast() else {
        return names;
    };
    each_statement(&module.body, &mut |stmt| {
        match stmt {
            ast::Stmt::Assign(assign) => {
                for target in &assign.targets {
                    collect_target_names(target, &mut names);
                }
            }
            ast::Stmt::AnnAssign(assign) => collect_target_names(&assign.target, &mut names),
            ast::Stmt::AugAssign(assign) => collect_target_names(&assign.target, &mut names),
            ast::Stmt::For(inner) => collect_target_names(&inner.target, &mut names),
            ast::Stmt::AsyncFor(inner) => collect_target_names(&inner.target, &mut names),
            ast::Stmt::With(inner) => {
                for item in &inner.items {
                    if let Some(vars) = &item.optional_vars {
                        collect_target_names(vars, &mut names);
                    }
                }
            }
            ast::Stmt::AsyncWith(inner) => {
                for item in &inner.items {
                    if let Some(vars) = &item.optional_vars {
                        collect_target_names(vars, &mut names);
                    }
                }
            }
            ast::Stmt::FunctionDef(def) => {
                names.insert(def.name.to_string());
                collect_parameter_names(&def.args, &mut names);
            }
            ast::Stmt::AsyncFunctionDef(def) => {
                names.insert(def.name.to_string());
                collect_parameter_names(&def.args, &mut names);
            }
            ast::Stmt::ClassDef(def) => {
                names.insert(def.name.to_string());
            }
            ast::Stmt::Try(inner) => {
                for handler in &inner.handlers {
                    let ast::ExceptHandler::ExceptHandler(handler) = handler;
                    if let Some(name) = &handler.name {
                        names.insert(name.to_string());
                    }
                }
            }
            ast::Stmt::TryStar(inner) => {
                for handler in &inner.handlers {
                    let ast::ExceptHandler::ExceptHandler(handler) = handler;
                    if let Some(name) = &handler.name {
                        names.insert(name.to_string());
                    }
                }
            }
            ast::Stmt::Match(inner) => {
                for case in &inner.cases {
                    let mut pattern_names = BTreeSet::new();
                    crate::frontend::names::collect_pattern_names(
                        &case.pattern,
                        &mut pattern_names,
                    );
                    names.extend(pattern_names);
                }
            }
            ast::Stmt::Global(inner) => {
                for name in &inner.names {
                    names.insert(name.to_string());
                }
            }
            ast::Stmt::Nonlocal(inner) => {
                for name in &inner.names {
                    names.insert(name.to_string());
                }
            }
            _ => {}
        }
        // Lambda parameters, comprehension targets, and walrus targets
        // bind inside expressions.
        for expr in statement_exprs(stmt) {
            walk_expr(expr, &mut |inner| match inner {
                ast::Expr::Lambda(lambda) => {
                    collect_parameter_names(&lambda.args, &mut names);
                }
                ast::Expr::NamedExpr(walrus) => {
                    collect_target_names(&walrus.target, &mut names);
                }
                ast::Expr::ListComp(comp) => collect_generator_names(&comp.generators, &mut names),
                ast::Expr::SetComp(comp) => collect_generator_names(&comp.generators, &mut names),
                ast::Expr::DictComp(comp) => collect_generator_names(&comp.generators, &mut names),
                ast::Expr::GeneratorExp(comp) => {
                    collect_generator_names(&comp.generators, &mut names);
                }
                _ => {}
            });
        }
    });
    names
}

fn collect_generator_names(generators: &[ast::Comprehension], names: &mut BTreeSet<String>) {
    for generator in generators {
        collect_target_names(&generator.target, names);
    }
}

/// Collects assignment-like target names into `names`.
fn collect_target_names(target: &ast::Expr, names: &mut BTreeSet<String>) {
    match target {
        ast::Expr::Name(name) => {
            names.insert(name.id.to_string());
        }
        ast::Expr::Tuple(tuple) => {
            for element in &tuple.elts {
                collect_target_names(element, names);
            }
        }
        ast::Expr::List(list) => {
            for element in &list.elts {
                collect_target_names(element, names);
            }
        }
        ast::Expr::Starred(starred) => collect_target_names(&starred.value, names),
        _ => {}
    }
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
