//! Post-parse syntax feature gate for `target-python` (DESIGN §5.2).
//!
//! rustpython-parser 0.4 always parses with its fixed Python 3.12 grammar
//! and has no `feature_version` pinning, so a construct newer than the
//! configured `target-python` parses without complaint. This module walks
//! the parsed AST after the fact and reports every construct whose minimum
//! Python version is above the configured target as an `MLC000` diagnostic
//! (error / certain, spanned on the construct). A gated file still has an
//! AST and facts, so analysis of the file — and of every other file —
//! continues; the gate only adds the diagnostic, mirroring how a file with
//! a real `MLC000` parse failure never stops the rest of the project.
//!
//! Per-construct coverage (each minimum version verified against `CPython`
//! 3.12 `ast.parse(..., feature_version=...)`, which rejects the construct
//! one minor version below and accepts it at the stated version):
//!
//! - `:=` assignment expression (PEP 572, Python 3.8): `ExprNamedExpr`.
//! - positional-only parameter marker `/` (PEP 570, Python 3.8):
//!   non-empty `Arguments::posonlyargs`, in `def`, `async def`, and
//!   `lambda`.
//! - `match` statement (PEP 634, Python 3.10): `StmtMatch`.
//! - `except*` handler (PEP 654, Python 3.11): `StmtTryStar`.
//! - `type` alias statement (PEP 695, Python 3.12): `StmtTypeAlias`.
//! - type parameter list on `def` / `class` (PEP 695, Python 3.12):
//!   non-empty `type_params`.
//!
//! NOT detectable from this AST (the parse result is identical with and
//! without the construct, so gating them would require a version-aware
//! tokenizer/grammar the bundled parser does not have):
//!
//! - parenthesized context managers, `with (a as x, b as y):`
//!   (Python 3.10): `WithItem` records no parenthesization.
//! - f-string self-documenting expressions, `f"{x=}"` (Python 3.8):
//!   rustpython-ast 0.4 desugars them into a literal plus a plain
//!   `FormattedValue` with no debug flag.
//! - constructs introduced before Python 3.8 (`async`/`await` 3.5,
//!   f-strings 3.6, underscores in numeric literals 3.6, ...): a
//!   `target-python` below 3.8 only gates the constructs listed above.

use rustpython_parser::ast::{self, Ranged};
use rustpython_parser::text_size::TextRange;

use crate::diagnostic::Diagnostic;
use crate::rules::registry;
use crate::source::SourceFile;

/// A Python version as `(major, minor)`; tuple ordering is version order.
pub type PythonVersion = (u32, u32);

/// Parses a `target-python` string (`"3.9"`) into a [`PythonVersion`].
///
/// Accepts exactly the `MAJOR.MINOR` shape the configuration loader
/// validates; anything else is `None`.
#[must_use]
pub fn parse_python_version(text: &str) -> Option<PythonVersion> {
    let (major, minor) = text.split_once('.')?;
    let number = |piece: &str| -> Option<u32> {
        (!piece.is_empty() && piece.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| piece.parse().ok())
            .flatten()
    };
    Some((number(major)?, number(minor)?))
}

/// A syntactic construct with a minimum Python version above 3.7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Feature {
    /// `:=` assignment expression (PEP 572).
    WalrusOperator,
    /// Positional-only parameter marker `/` (PEP 570).
    PositionalOnlyParameters,
    /// `match` statement (PEP 634).
    MatchStatement,
    /// `except*` handler (PEP 654).
    ExceptStar,
    /// `type` alias statement (PEP 695).
    TypeAliasStatement,
    /// Type parameter list on `def` / `class` (PEP 695).
    TypeParameterList,
}

impl Feature {
    /// First Python version that accepts the construct (verified against
    /// `CPython` 3.12 `ast.parse(feature_version=...)`).
    #[must_use]
    pub const fn introduced_in(self) -> PythonVersion {
        match self {
            Self::WalrusOperator | Self::PositionalOnlyParameters => (3, 8),
            Self::MatchStatement => (3, 10),
            Self::ExceptStar => (3, 11),
            Self::TypeAliasStatement | Self::TypeParameterList => (3, 12),
        }
    }

    /// Human-readable construct name for diagnostics.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::WalrusOperator => "assignment expression `:=`",
            Self::PositionalOnlyParameters => "positional-only parameter marker `/`",
            Self::MatchStatement => "`match` statement",
            Self::ExceptStar => "`except*` handler",
            Self::TypeAliasStatement => "`type` alias statement",
            Self::TypeParameterList => "type parameter list",
        }
    }
}

/// One gated construct found in a module.
#[derive(Debug, Clone, Copy)]
pub struct Violation {
    /// Which construct was found.
    pub feature: Feature,
    /// Byte range of the construct in the decoded text.
    pub range: TextRange,
}

/// Highest [`Feature::introduced_in`] version any feature carries; a
/// target at or above it can never produce a violation.
const NEWEST_GATED_FEATURE: PythonVersion = (3, 12);

/// Walks a parsed module and returns every construct whose minimum Python
/// version is above `target`, in source order.
#[must_use]
pub fn violations(module: &ast::ModModule, target: PythonVersion) -> Vec<Violation> {
    if target >= NEWEST_GATED_FEATURE {
        return Vec::new();
    }
    let mut walker = Walker {
        target,
        out: Vec::new(),
    };
    walker.stmts(&module.body);
    walker.out
}

/// Runs the gate over one parsed file, mapping violations to `MLC000`
/// diagnostics. A file without an AST (decode or parse failure) and an
/// unparseable `target-python` (impossible after config validation) gate
/// nothing.
#[must_use]
pub fn gate(file: &SourceFile, target_python: &str) -> Vec<Diagnostic> {
    let Some(target) = parse_python_version(target_python) else {
        debug_assert!(false, "target-python is validated at config time");
        return Vec::new();
    };
    let Some(module) = file.ast() else {
        return Vec::new();
    };
    violations(module, target)
        .into_iter()
        .map(|violation| {
            let metadata = &registry::SYNTAX_ERROR;
            let (major, minor) = violation.feature.introduced_in();
            Diagnostic {
                rule_id: metadata.id.to_owned(),
                severity: metadata.default_severity,
                confidence: metadata.minimum_confidence,
                path: file.relative_path().to_owned(),
                primary_span: file.span_of_range(violation.range),
                message: format!(
                    "{label} requires Python {major}.{minor} but target-python is {target_python}",
                    label = violation.feature.label(),
                ),
                explanation: Some(
                    "The construct parses with the bundled grammar but the configured \
                     target-python cannot run it. Analysis of this file and of every \
                     other selected file still continues."
                        .to_owned(),
                ),
                related_locations: Vec::new(),
                evidence: std::collections::BTreeMap::new(),
                estimated_cost: None,
                applicable_profiles: Vec::new(),
                fix: None,
            }
        })
        .collect()
}

/// Exhaustive AST walk collecting gated constructs.
struct Walker {
    target: PythonVersion,
    out: Vec<Violation>,
}

impl Walker {
    fn flag(&mut self, feature: Feature, range: TextRange) {
        if feature.introduced_in() > self.target {
            self.out.push(Violation { feature, range });
        }
    }

    fn stmts(&mut self, stmts: &[ast::Stmt]) {
        for stmt in stmts {
            self.stmt(stmt);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one arm per Stmt variant; splitting hides the exhaustiveness"
    )]
    fn stmt(&mut self, stmt: &ast::Stmt) {
        match stmt {
            ast::Stmt::FunctionDef(def) => {
                self.type_params(&def.type_params);
                self.arguments(&def.args);
                self.exprs(&def.decorator_list);
                self.opt_expr(def.returns.as_deref());
                self.stmts(&def.body);
            }
            ast::Stmt::AsyncFunctionDef(def) => {
                self.type_params(&def.type_params);
                self.arguments(&def.args);
                self.exprs(&def.decorator_list);
                self.opt_expr(def.returns.as_deref());
                self.stmts(&def.body);
            }
            ast::Stmt::ClassDef(def) => {
                self.type_params(&def.type_params);
                self.exprs(&def.bases);
                self.keywords(&def.keywords);
                self.exprs(&def.decorator_list);
                self.stmts(&def.body);
            }
            ast::Stmt::TypeAlias(alias) => {
                self.flag(Feature::TypeAliasStatement, alias.range);
                self.expr(&alias.name);
                self.type_params(&alias.type_params);
                self.expr(&alias.value);
            }
            ast::Stmt::Match(subject) => {
                self.flag(Feature::MatchStatement, subject.range);
                self.expr(&subject.subject);
                for case in &subject.cases {
                    self.pattern(&case.pattern);
                    self.opt_expr(case.guard.as_deref());
                    self.stmts(&case.body);
                }
            }
            ast::Stmt::TryStar(try_star) => {
                // Span the first `except*` handler (the construct itself)
                // rather than the whole try block.
                let range = try_star
                    .handlers
                    .first()
                    .map_or(try_star.range, Ranged::range);
                self.flag(Feature::ExceptStar, range);
                self.stmts(&try_star.body);
                self.handlers(&try_star.handlers);
                self.stmts(&try_star.orelse);
                self.stmts(&try_star.finalbody);
            }
            ast::Stmt::Try(try_stmt) => {
                self.stmts(&try_stmt.body);
                self.handlers(&try_stmt.handlers);
                self.stmts(&try_stmt.orelse);
                self.stmts(&try_stmt.finalbody);
            }
            ast::Stmt::Return(ret) => self.opt_expr(ret.value.as_deref()),
            ast::Stmt::Delete(delete) => self.exprs(&delete.targets),
            ast::Stmt::Assign(assign) => {
                self.exprs(&assign.targets);
                self.expr(&assign.value);
            }
            ast::Stmt::AugAssign(assign) => {
                self.expr(&assign.target);
                self.expr(&assign.value);
            }
            ast::Stmt::AnnAssign(assign) => {
                self.expr(&assign.target);
                self.expr(&assign.annotation);
                self.opt_expr(assign.value.as_deref());
            }
            ast::Stmt::For(for_stmt) => {
                self.expr(&for_stmt.target);
                self.expr(&for_stmt.iter);
                self.stmts(&for_stmt.body);
                self.stmts(&for_stmt.orelse);
            }
            ast::Stmt::AsyncFor(for_stmt) => {
                self.expr(&for_stmt.target);
                self.expr(&for_stmt.iter);
                self.stmts(&for_stmt.body);
                self.stmts(&for_stmt.orelse);
            }
            ast::Stmt::While(while_stmt) => {
                self.expr(&while_stmt.test);
                self.stmts(&while_stmt.body);
                self.stmts(&while_stmt.orelse);
            }
            ast::Stmt::If(if_stmt) => {
                self.expr(&if_stmt.test);
                self.stmts(&if_stmt.body);
                self.stmts(&if_stmt.orelse);
            }
            ast::Stmt::With(with_stmt) => {
                self.with_items(&with_stmt.items);
                self.stmts(&with_stmt.body);
            }
            ast::Stmt::AsyncWith(with_stmt) => {
                self.with_items(&with_stmt.items);
                self.stmts(&with_stmt.body);
            }
            ast::Stmt::Raise(raise) => {
                self.opt_expr(raise.exc.as_deref());
                self.opt_expr(raise.cause.as_deref());
            }
            ast::Stmt::Assert(assert) => {
                self.expr(&assert.test);
                self.opt_expr(assert.msg.as_deref());
            }
            ast::Stmt::Expr(expr) => self.expr(&expr.value),
            ast::Stmt::Import(_)
            | ast::Stmt::ImportFrom(_)
            | ast::Stmt::Global(_)
            | ast::Stmt::Nonlocal(_)
            | ast::Stmt::Pass(_)
            | ast::Stmt::Break(_)
            | ast::Stmt::Continue(_) => {}
        }
    }

    fn handlers(&mut self, handlers: &[ast::ExceptHandler]) {
        for handler in handlers {
            let ast::ExceptHandler::ExceptHandler(handler) = handler;
            self.opt_expr(handler.type_.as_deref());
            self.stmts(&handler.body);
        }
    }

    fn with_items(&mut self, items: &[ast::WithItem]) {
        for item in items {
            self.expr(&item.context_expr);
            self.opt_expr(item.optional_vars.as_deref());
        }
    }

    fn type_params(&mut self, params: &[ast::TypeParam]) {
        if let (Some(first), Some(last)) = (params.first(), params.last()) {
            self.flag(
                Feature::TypeParameterList,
                TextRange::new(first.range().start(), last.range().end()),
            );
        }
        for param in params {
            if let ast::TypeParam::TypeVar(type_var) = param {
                self.opt_expr(type_var.bound.as_deref());
            }
        }
    }

    fn arguments(&mut self, args: &ast::Arguments) {
        if let (Some(first), Some(last)) = (args.posonlyargs.first(), args.posonlyargs.last()) {
            self.flag(
                Feature::PositionalOnlyParameters,
                TextRange::new(first.def.range.start(), last.def.range.end()),
            );
        }
        for arg in args
            .posonlyargs
            .iter()
            .chain(&args.args)
            .chain(&args.kwonlyargs)
        {
            self.opt_expr(arg.def.annotation.as_deref());
            self.opt_expr(arg.default.as_deref());
        }
        for arg in [args.vararg.as_deref(), args.kwarg.as_deref()]
            .into_iter()
            .flatten()
        {
            self.opt_expr(arg.annotation.as_deref());
        }
    }

    fn keywords(&mut self, keywords: &[ast::Keyword]) {
        for keyword in keywords {
            self.expr(&keyword.value);
        }
    }

    fn comprehensions(&mut self, generators: &[ast::Comprehension]) {
        for generator in generators {
            self.expr(&generator.target);
            self.expr(&generator.iter);
            self.exprs(&generator.ifs);
        }
    }

    fn exprs(&mut self, exprs: &[ast::Expr]) {
        for expr in exprs {
            self.expr(expr);
        }
    }

    fn opt_expr(&mut self, expr: Option<&ast::Expr>) {
        if let Some(expr) = expr {
            self.expr(expr);
        }
    }

    fn expr(&mut self, expr: &ast::Expr) {
        match expr {
            ast::Expr::NamedExpr(named) => {
                self.flag(Feature::WalrusOperator, named.range);
                self.expr(&named.target);
                self.expr(&named.value);
            }
            ast::Expr::BoolOp(op) => self.exprs(&op.values),
            ast::Expr::BinOp(op) => {
                self.expr(&op.left);
                self.expr(&op.right);
            }
            ast::Expr::UnaryOp(op) => self.expr(&op.operand),
            ast::Expr::Lambda(lambda) => {
                self.arguments(&lambda.args);
                self.expr(&lambda.body);
            }
            ast::Expr::IfExp(if_exp) => {
                self.expr(&if_exp.test);
                self.expr(&if_exp.body);
                self.expr(&if_exp.orelse);
            }
            ast::Expr::Dict(dict) => {
                for key in dict.keys.iter().flatten() {
                    self.expr(key);
                }
                self.exprs(&dict.values);
            }
            ast::Expr::Set(set) => self.exprs(&set.elts),
            ast::Expr::ListComp(comp) => {
                self.expr(&comp.elt);
                self.comprehensions(&comp.generators);
            }
            ast::Expr::SetComp(comp) => {
                self.expr(&comp.elt);
                self.comprehensions(&comp.generators);
            }
            ast::Expr::DictComp(comp) => {
                self.expr(&comp.key);
                self.expr(&comp.value);
                self.comprehensions(&comp.generators);
            }
            ast::Expr::GeneratorExp(comp) => {
                self.expr(&comp.elt);
                self.comprehensions(&comp.generators);
            }
            ast::Expr::Await(await_expr) => self.expr(&await_expr.value),
            ast::Expr::Yield(yield_expr) => self.opt_expr(yield_expr.value.as_deref()),
            ast::Expr::YieldFrom(yield_from) => self.expr(&yield_from.value),
            ast::Expr::Compare(compare) => {
                self.expr(&compare.left);
                self.exprs(&compare.comparators);
            }
            ast::Expr::Call(call) => {
                self.expr(&call.func);
                self.exprs(&call.args);
                self.keywords(&call.keywords);
            }
            ast::Expr::FormattedValue(value) => {
                self.expr(&value.value);
                self.opt_expr(value.format_spec.as_deref());
            }
            ast::Expr::JoinedStr(joined) => self.exprs(&joined.values),
            ast::Expr::Attribute(attribute) => self.expr(&attribute.value),
            ast::Expr::Subscript(subscript) => {
                self.expr(&subscript.value);
                self.expr(&subscript.slice);
            }
            ast::Expr::Starred(starred) => self.expr(&starred.value),
            ast::Expr::List(list) => self.exprs(&list.elts),
            ast::Expr::Tuple(tuple) => self.exprs(&tuple.elts),
            ast::Expr::Slice(slice) => {
                self.opt_expr(slice.lower.as_deref());
                self.opt_expr(slice.upper.as_deref());
                self.opt_expr(slice.step.as_deref());
            }
            ast::Expr::Constant(_) | ast::Expr::Name(_) => {}
        }
    }

    fn pattern(&mut self, pattern: &ast::Pattern) {
        match pattern {
            ast::Pattern::MatchValue(value) => self.expr(&value.value),
            ast::Pattern::MatchSingleton(_) | ast::Pattern::MatchStar(_) => {}
            ast::Pattern::MatchSequence(sequence) => self.patterns(&sequence.patterns),
            ast::Pattern::MatchMapping(mapping) => {
                self.exprs(&mapping.keys);
                self.patterns(&mapping.patterns);
            }
            ast::Pattern::MatchClass(class) => {
                self.expr(&class.cls);
                self.patterns(&class.patterns);
                self.patterns(&class.kwd_patterns);
            }
            ast::Pattern::MatchAs(as_pattern) => {
                if let Some(inner) = &as_pattern.pattern {
                    self.pattern(inner);
                }
            }
            ast::Pattern::MatchOr(or_pattern) => self.patterns(&or_pattern.patterns),
        }
    }

    fn patterns(&mut self, patterns: &[ast::Pattern]) {
        for pattern in patterns {
            self.pattern(pattern);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use crate::source::SourceManager;

    fn parse(text: &str) -> ast::ModModule {
        match rustpython_parser::parse(text, rustpython_parser::Mode::Module, "test.py") {
            Ok(ast::Mod::Module(module)) => module,
            other => panic!("test source must parse: {other:?}"),
        }
    }

    fn found(text: &str, target: PythonVersion) -> Vec<Feature> {
        violations(&parse(text), target)
            .into_iter()
            .map(|violation| violation.feature)
            .collect()
    }

    #[test]
    fn match_statement_is_gated_below_3_10() {
        let source = "match command:\n    case 1:\n        pass\n";
        assert_eq!(found(source, (3, 9)), [Feature::MatchStatement]);
        assert_eq!(found(source, (3, 10)), []);
    }

    #[test]
    fn except_star_is_gated_below_3_11() {
        let source = "try:\n    pass\nexcept* ValueError:\n    pass\n";
        assert_eq!(found(source, (3, 10)), [Feature::ExceptStar]);
        assert_eq!(found(source, (3, 11)), []);
    }

    #[test]
    fn pep695_type_alias_and_type_params_are_gated_below_3_12() {
        assert_eq!(
            found("type Vector = list[float]\n", (3, 11)),
            [Feature::TypeAliasStatement]
        );
        assert_eq!(
            found(
                "def first[T](items: list[T]) -> T:\n    return items[0]\n",
                (3, 11)
            ),
            [Feature::TypeParameterList]
        );
        assert_eq!(
            found("class Box[T]:\n    pass\n", (3, 11)),
            [Feature::TypeParameterList]
        );
        assert_eq!(found("class Box[T]:\n    pass\n", (3, 12)), []);
    }

    #[test]
    fn walrus_and_positional_only_are_gated_below_3_8() {
        assert_eq!(
            found("if (n := 10) > 5:\n    pass\n", (3, 7)),
            [Feature::WalrusOperator]
        );
        assert_eq!(found("if (n := 10) > 5:\n    pass\n", (3, 8)), []);
        assert_eq!(
            found("def f(a, /, b):\n    pass\n", (3, 7)),
            [Feature::PositionalOnlyParameters]
        );
        assert_eq!(
            found("f = lambda a, /, b: a\n", (3, 7)),
            [Feature::PositionalOnlyParameters]
        );
        assert_eq!(found("def f(a, /, b):\n    pass\n", (3, 8)), []);
    }

    #[test]
    fn nested_constructs_are_found() {
        // A walrus inside a comprehension inside a decorator's call inside
        // a class body proves the walk descends everywhere.
        let source =
            "class C:\n    @deco([y for x in items if (y := x)])\n    def m(self):\n        pass\n";
        assert_eq!(found(source, (3, 7)), [Feature::WalrusOperator]);
        // Inside f-string format specs and dict values too.
        assert_eq!(
            found("x = {\"k\": f\"{(v := 1):>{width}}\"}\n", (3, 7)),
            [Feature::WalrusOperator]
        );
    }

    #[test]
    fn ordinary_modern_free_code_is_clean_at_the_floor() {
        let source = "\
from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        with open(\"x\") as handle:
            data = handle.read()
        self.play(FadeIn(square))
";
        assert_eq!(found(source, (3, 0)), []);
    }

    #[test]
    fn gate_maps_violations_to_mlc000_with_construct_span() {
        let mut sources = SourceManager::new("/project");
        sources.load_bytes(
            Path::new("/project/scene.py"),
            b"x = 1\nmatch x:\n    case 1:\n        pass\n",
        );
        let file = &sources.files()[0];
        let diagnostics = gate(file, "3.9");
        assert_eq!(diagnostics.len(), 1);
        let diagnostic = &diagnostics[0];
        assert_eq!(diagnostic.rule_id, "MLC000");
        assert_eq!(
            diagnostic.message,
            "`match` statement requires Python 3.10 but target-python is 3.9"
        );
        assert_eq!(diagnostic.primary_span.start.line, 2);
        assert_eq!(diagnostic.primary_span.start.column, 1);
        // The file still parsed: the gate never removes the AST or facts.
        assert!(file.is_parsed());
        assert!(gate(file, "3.10").is_empty());
    }
}
