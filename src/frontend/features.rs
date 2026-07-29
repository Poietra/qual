//! Post-parse syntax feature gate for `target-python` (DESIGN §5.2).
//!
//! rustpython-parser 0.4 always parses with its fixed Python 3.12 grammar
//! and has no `feature_version` pinning, so a construct newer than the
//! configured `target-python` parses without complaint. This module walks
//! the parsed AST, the token stream, and the raw text of f-string tokens
//! after the fact and reports every construct whose minimum Python version
//! is above the configured target as an `MLC000` diagnostic (error /
//! certain, spanned on the construct). A gated file still has an AST and
//! facts, so analysis of the file — and of every other file — continues;
//! the gate only adds the diagnostic, mirroring how a file with a real
//! `MLC000` parse failure never stops the rest of the project.
//!
//! # Contract and floor
//!
//! The gate guarantees exactly this: a file that passes the gate silently
//! is *parseable* by the target version's own parser (`ast.parse`). The
//! guarantee is complete for every `target-python` from
//! [`crate::config::loader::MIN_TARGET_PYTHON`] (3.6) up to the bundled
//! grammar (3.12): every parse-level construct introduced after 3.6 that
//! the bundled parser accepts is either detected below or is a bundled
//! parse error itself (so it already reports `MLC000`). Targets below 3.6
//! are configuration errors, because completeness below 3.6 would require
//! gating every construct Python 3.6 added — f-strings, numeric-literal
//! underscores, variable annotations, async generators and comprehensions,
//! and trailing commas after `*args`/`**kwargs` in signatures (bpo-9232) —
//! and neither f-strings nor the trailing-comma form are gated by
//! `CPython`'s `ast.parse(feature_version=...)`, so a sub-3.6 guarantee
//! could not be oracle-verified.
//!
//! # Coverage (oracle: `CPython` 3.12 `ast.parse(feature_version=(3, N))`)
//!
//! | construct | min | detection | oracle |
//! |---|---|---|---|
//! | `async`/`await` syntax outside `async def` (await, `async for`/`with`, async comprehension) | 3.7 | AST + async-context | fv 3.6 rejects, 3.7 accepts |
//! | `:=` assignment expression (PEP 572) | 3.8 | AST | fv 3.7 rejects, 3.8 accepts |
//! | positional-only `/` (PEP 570), incl. lambdas | 3.8 | AST | fv 3.7 rejects, 3.8 accepts |
//! | f-string self-documenting `=` (bpo-36817) | 3.8 | f-string token text | NOT fv-gated (fv 3.7 accepts `f"{x=}"`); dated from the 3.8 release notes |
//! | relaxed decorator expressions (PEP 614) | 3.9 | token stream | NOT fv-gated; dated from PEP 614 |
//! | parenthesized context managers with `as` | 3.9 | token stream | NOT fv-gated; accepted by 3.9's PEG parser, documented in 3.10 — dated 3.9 so a 3.9 target that runs it stays silent |
//! | `match` statement (PEP 634) | 3.10 | AST | fv 3.9 rejects, 3.10 accepts |
//! | `except*` handler (PEP 654) | 3.11 | AST | fv 3.10 rejects, 3.11 accepts |
//! | `*` unpacking in subscripts / `*args` annotations (PEP 646) | 3.11 | AST | NOT fv-gated (fv 3.10 accepts `x[*a]`); dated from PEP 646 |
//! | `type` alias statement (PEP 695) | 3.12 | AST | fv 3.11 rejects, 3.12 accepts |
//! | type parameter list on `def`/`class` (PEP 695) | 3.12 | AST | fv 3.11 rejects, 3.12 accepts |
//! | f-string expression containing a backslash, newline, or `#` (PEP 701) | 3.12 | f-string token text | NOT fv-gated; dated from PEP 701 |
//!
//! PEP 701 forms the bundled parser rejects (quote reuse such as
//! `f"{"x"}"`) are ordinary parse errors and need no gate entry.
//!
//! # Outside the contract (compile-stage, not parse-stage)
//!
//! Version-dependent restrictions that the affected versions' *parsers*
//! accept and only their compilers reject are not gated, because the gate
//! guarantee is parseability: `continue` inside `finally` (compile error
//! before 3.8; fv 3.7 accepts), `yield` inside `async def` (compile error
//! before 3.6), and the 3.11 relaxations of where async comprehensions
//! and `await` may sit inside nested comprehensions. A construct in this
//! category never fires the gate (DESIGN §15: silence over a false
//! positive on something the target's parser accepts).

use rustpython_parser::ast::Ranged;
use rustpython_parser::text_size::TextRange;
use rustpython_parser::{Mode, StringKind, Tok, ast, lexer};

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

/// A syntactic construct with a minimum Python version above the floor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Feature {
    /// `await`, `async for`, `async with`, or an async comprehension
    /// lexically outside every `async def` (keywords only there from 3.7).
    AsyncOutsideAsyncFunction,
    /// `:=` assignment expression (PEP 572).
    WalrusOperator,
    /// Positional-only parameter marker `/` (PEP 570).
    PositionalOnlyParameters,
    /// f-string self-documenting `=` expression (`f"{x=}"`, bpo-36817).
    FStringSelfDocumenting,
    /// Decorator that is not a plain dotted name or dotted-name call
    /// (PEP 614).
    RelaxedDecorator,
    /// Parenthesized context managers with `as` bindings,
    /// `with (a as x, b as y):`.
    ParenthesizedContextManagers,
    /// `match` statement (PEP 634).
    MatchStatement,
    /// `except*` handler (PEP 654).
    ExceptStar,
    /// `*` unpacking in a subscript or `*args` annotation (PEP 646).
    SubscriptUnpack,
    /// `type` alias statement (PEP 695).
    TypeAliasStatement,
    /// Type parameter list on `def` / `class` (PEP 695).
    TypeParameterList,
    /// f-string whose expression part contains a backslash, newline, or
    /// `#` comment (PEP 701).
    ModernFStringExpression,
}

impl Feature {
    /// First Python version whose parser accepts the construct (see the
    /// module-level coverage table for how each minimum was verified).
    #[must_use]
    pub const fn introduced_in(self) -> PythonVersion {
        match self {
            Self::AsyncOutsideAsyncFunction => (3, 7),
            Self::WalrusOperator
            | Self::PositionalOnlyParameters
            | Self::FStringSelfDocumenting => (3, 8),
            Self::RelaxedDecorator | Self::ParenthesizedContextManagers => (3, 9),
            Self::MatchStatement => (3, 10),
            Self::ExceptStar | Self::SubscriptUnpack => (3, 11),
            Self::TypeAliasStatement | Self::TypeParameterList | Self::ModernFStringExpression => {
                (3, 12)
            }
        }
    }

    /// Human-readable construct name for diagnostics.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::AsyncOutsideAsyncFunction => "`async`/`await` syntax outside `async def`",
            Self::WalrusOperator => "assignment expression `:=`",
            Self::PositionalOnlyParameters => "positional-only parameter marker `/`",
            Self::FStringSelfDocumenting => "f-string self-documenting `=` expression",
            Self::RelaxedDecorator => "decorator beyond a dotted name or call",
            Self::ParenthesizedContextManagers => "parenthesized context managers with `as`",
            Self::MatchStatement => "`match` statement",
            Self::ExceptStar => "`except*` handler",
            Self::SubscriptUnpack => "unpacking `*` in a subscript or annotation",
            Self::TypeAliasStatement => "`type` alias statement",
            Self::TypeParameterList => "type parameter list",
            Self::ModernFStringExpression => {
                "f-string expression containing a backslash, newline, or `#`"
            }
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

/// Lexes `text` the way [`crate::source::SourceManager`] does: tokens up
/// to the first lexical error. Used by fix application, which must gate
/// fixed text that has no [`SourceFile`] behind it.
#[must_use]
pub fn lex_tokens(text: &str) -> Vec<(Tok, TextRange)> {
    let mut tokens = Vec::new();
    for item in lexer::lex(text, Mode::Module) {
        match item {
            Ok(token) => tokens.push(token),
            Err(_) => break,
        }
    }
    tokens
}

/// Walks a parsed module, its token stream, and its f-string token text,
/// returning every construct whose minimum Python version is above
/// `target`, in source order.
#[must_use]
pub fn violations(
    tokens: &[(Tok, TextRange)],
    module: &ast::ModModule,
    target: PythonVersion,
) -> Vec<Violation> {
    if target >= NEWEST_GATED_FEATURE {
        return Vec::new();
    }
    let mut walker = Walker {
        target,
        async_function_depth: 0,
        out: Vec::new(),
    };
    walker.stmts(&module.body);
    let mut out = walker.out;
    token_violations(tokens, target, &mut out);
    out.sort_by_key(|violation| (violation.range.start(), violation.range.end()));
    out
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
    violations(file.tokens(), module, target)
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

/// Appends the reverse-direction `target-python` hint to a parse-failure
/// `MLC000` diagnostic (DESIGN §7.1 honesty, roadmap remnant).
///
/// The bundled parser uses the fixed 3.12 grammar, where `async` and
/// `await` are hard keywords. Python 3.6 still treated them as soft
/// keywords, so source that legally uses them as identifiers (`async = 1`)
/// is valid for a 3.6 target yet fails to parse here. When the configured
/// target predates the 3.7 reservation, the file failed to parse, and the
/// failing line (or the parser's own message) mentions `async` / `await`
/// as a word, the diagnostic gains a hedged note — a hint, not a claim,
/// because the file may be broken for other reasons too. Anything else
/// (parsed files, newer targets, decode failures with no text) is left
/// untouched.
pub fn append_pre37_async_hint(
    file: &SourceFile,
    target_python: &str,
    diagnostic: &mut crate::diagnostic::Diagnostic,
) {
    const HINT: &str = " (note: this may be valid Python 3.6 source; qual \
                        parses with the 3.12 grammar, where `async`/`await` are \
                        reserved keywords)";
    let Some(target) = parse_python_version(target_python) else {
        return;
    };
    if target >= (3, 7) || file.is_parsed() {
        return;
    }
    let mentions = |text: &str| contains_word(text, "async") || contains_word(text, "await");
    let line = diagnostic.primary_span.start.line;
    if mentions(file.line_text(line)) || mentions(&diagnostic.message) {
        diagnostic.message.push_str(HINT);
    }
}

/// Whether `text` contains `word` delimited by non-identifier characters.
fn contains_word(text: &str, word: &str) -> bool {
    let bytes = text.as_bytes();
    let is_ident = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    let mut start = 0;
    while let Some(position) = text[start..].find(word) {
        let begin = start + position;
        let end = begin + word.len();
        let before_ok = begin == 0 || !is_ident(bytes[begin - 1]);
        let after_ok = end >= bytes.len() || !is_ident(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        start = begin + 1;
    }
    false
}

/// Exhaustive AST walk collecting gated constructs.
struct Walker {
    target: PythonVersion,
    /// Number of enclosing `async def` scopes. The 3.5/3.6 tokenizer kept
    /// `async`/`await` keywords active anywhere inside an `async def`
    /// block, including nested plain `def`s (verified with the oracle:
    /// `feature_version=(3, 6)` accepts `await` in a `def` nested in an
    /// `async def`), so a plain `def` does not reset this counter.
    async_function_depth: u32,
    out: Vec<Violation>,
}

impl Walker {
    fn flag(&mut self, feature: Feature, range: TextRange) {
        if feature.introduced_in() > self.target {
            self.out.push(Violation { feature, range });
        }
    }

    /// Flags `async`/`await` syntax when no `async def` encloses it: the
    /// pre-3.7 tokenizer only recognized the keywords inside `async def`,
    /// so everywhere else the construct is a 3.6 parse error.
    fn flag_async_outside(&mut self, range: TextRange) {
        if self.async_function_depth == 0 {
            self.flag(Feature::AsyncOutsideAsyncFunction, range);
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
                // The whole statement — header included — sits in the
                // async region the 3.5/3.6 tokenizer opened at `async def`.
                self.async_function_depth += 1;
                self.type_params(&def.type_params);
                self.arguments(&def.args);
                self.exprs(&def.decorator_list);
                self.opt_expr(def.returns.as_deref());
                self.stmts(&def.body);
                self.async_function_depth -= 1;
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
                // Span the header (`async for target in iter`), not the body.
                let range = TextRange::new(for_stmt.range.start(), for_stmt.iter.range().end());
                self.flag_async_outside(range);
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
                let header_end = with_stmt
                    .items
                    .last()
                    .map_or(with_stmt.range.start(), |item| {
                        item.optional_vars
                            .as_deref()
                            .map_or(item.context_expr.range(), Ranged::range)
                            .end()
                    });
                self.flag_async_outside(TextRange::new(with_stmt.range.start(), header_end));
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
            // PEP 646: `def f(*args: *Ts)` — a starred vararg annotation.
            if let Some(ast::Expr::Starred(starred)) = arg.annotation.as_deref() {
                self.flag(Feature::SubscriptUnpack, starred.range);
            }
            self.opt_expr(arg.annotation.as_deref());
        }
    }

    fn keywords(&mut self, keywords: &[ast::Keyword]) {
        for keyword in keywords {
            self.expr(&keyword.value);
        }
    }

    /// Flags async comprehension clauses outside `async def` and then
    /// walks the clauses. The comprehension's own range spans the flag
    /// because `Comprehension` nodes carry no range of their own.
    fn comprehensions(&mut self, generators: &[ast::Comprehension], comp_range: TextRange) {
        if generators.iter().any(|generator| generator.is_async) {
            self.flag_async_outside(comp_range);
        }
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
                self.comprehensions(&comp.generators, comp.range);
            }
            ast::Expr::SetComp(comp) => {
                self.expr(&comp.elt);
                self.comprehensions(&comp.generators, comp.range);
            }
            ast::Expr::DictComp(comp) => {
                self.expr(&comp.key);
                self.expr(&comp.value);
                self.comprehensions(&comp.generators, comp.range);
            }
            ast::Expr::GeneratorExp(comp) => {
                self.expr(&comp.elt);
                self.comprehensions(&comp.generators, comp.range);
            }
            ast::Expr::Await(await_expr) => {
                self.flag_async_outside(await_expr.range);
                self.expr(&await_expr.value);
            }
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
                // PEP 646: `x[*a]` and `x[a, *b]` — `*` unpacking directly
                // in the subscript. Deeper starred expressions (inside a
                // nested display, say) predate 3.11 and stay silent.
                match subscript.slice.as_ref() {
                    ast::Expr::Starred(starred) => {
                        self.flag(Feature::SubscriptUnpack, starred.range);
                    }
                    ast::Expr::Tuple(tuple) => {
                        for elt in &tuple.elts {
                            if let ast::Expr::Starred(starred) = elt {
                                self.flag(Feature::SubscriptUnpack, starred.range);
                            }
                        }
                    }
                    _ => {}
                }
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

// ---------------------------------------------------------------------------
// Token-stream detection: constructs the AST cannot represent.
// ---------------------------------------------------------------------------

/// Whether a token is trivia the decorator and with-header scans skip.
const fn is_trivia(token: &Tok) -> bool {
    matches!(token, Tok::Comment(_) | Tok::NonLogicalNewline)
}

/// Whether a token can precede a statement-level `@` (a decorator line
/// start). At bracket depth 0 a `NonLogicalNewline` only follows blank or
/// comment-only lines, so it is a line start too; binary `@` continuation
/// at depth 0 requires a backslash join, which emits no newline token.
const fn starts_logical_line(previous: Option<&Tok>) -> bool {
    matches!(
        previous,
        None | Some(
            Tok::Newline | Tok::NonLogicalNewline | Tok::Indent | Tok::Dedent | Tok::Comment(_)
        )
    )
}

/// Scans the token stream for gated constructs invisible in the AST:
/// f-string text features, parenthesized context managers with `as`, and
/// relaxed decorator expressions.
fn token_violations(tokens: &[(Tok, TextRange)], target: PythonVersion, out: &mut Vec<Violation>) {
    let mut flag = |feature: Feature, range: TextRange| {
        if feature.introduced_in() > target {
            out.push(Violation { feature, range });
        }
    };
    let mut bracket_depth = 0_u32;
    let mut previous: Option<&Tok> = None;
    for (index, (token, range)) in tokens.iter().enumerate() {
        match token {
            Tok::Lpar | Tok::Lsqb | Tok::Lbrace => bracket_depth += 1,
            Tok::Rpar | Tok::Rsqb | Tok::Rbrace => {
                bracket_depth = bracket_depth.saturating_sub(1);
            }
            Tok::String {
                value,
                kind: StringKind::FString | StringKind::RawFString,
                ..
            } => {
                if let Some(findings) = scan_fstring(value.as_bytes()) {
                    if findings.self_documenting {
                        flag(Feature::FStringSelfDocumenting, *range);
                    }
                    if findings.modern_expression {
                        flag(Feature::ModernFStringExpression, *range);
                    }
                }
            }
            Tok::With => {
                if let Some(found) = parenthesized_with_as(tokens, index) {
                    flag(Feature::ParenthesizedContextManagers, found);
                }
            }
            Tok::At if bracket_depth == 0 && starts_logical_line(previous) => {
                if let Some(found) = relaxed_decorator(tokens, index) {
                    flag(Feature::RelaxedDecorator, found);
                }
            }
            _ => {}
        }
        previous = Some(token);
    }
}

/// Detects `as` bindings inside a parenthesized `with` header. `with`
/// followed by `(` whose balanced region contains an `as` token cannot be
/// the pre-3.9 "single parenthesized expression" reading — expressions
/// have no `as` — so it is the 3.9+ parenthesized-context-managers form.
/// `with (a, b):` (tuple grouping) and `with (a) as b:` parse on every
/// supported version and stay silent.
fn parenthesized_with_as(tokens: &[(Tok, TextRange)], with_index: usize) -> Option<TextRange> {
    let mut cursor = tokens[with_index + 1..]
        .iter()
        .filter(|(token, _)| !is_trivia(token));
    let &(Tok::Lpar, open_range) = cursor.next()? else {
        return None;
    };
    let mut depth = 1_u32;
    let mut has_as = false;
    for (token, range) in cursor {
        match token {
            Tok::Lpar | Tok::Lsqb | Tok::Lbrace => depth += 1,
            Tok::Rpar | Tok::Rsqb | Tok::Rbrace => {
                depth -= 1;
                if depth == 0 {
                    return has_as.then(|| TextRange::new(open_range.start(), range.end()));
                }
            }
            Tok::As => has_as = true,
            _ => {}
        }
    }
    None // unbalanced header: bail silently (cannot happen in parsed code)
}

/// Checks the decorator starting at the `@` token against the pre-3.9
/// grammar `'@' dotted_name [ '(' [arglist] ')' ] NEWLINE` and returns
/// the offending span when the decorator needs PEP 614.
fn relaxed_decorator(tokens: &[(Tok, TextRange)], at_index: usize) -> Option<TextRange> {
    let at_start = tokens[at_index].1.start();
    let relaxed = |range: TextRange| Some(TextRange::new(at_start, range.end().max(at_start)));
    let mut rest = tokens[at_index + 1..]
        .iter()
        .filter(|(token, _)| !is_trivia(token))
        .map(|(token, range)| (token, *range));
    // dotted_name: NAME ('.' NAME)*
    let (mut token, mut range) = rest.next()?;
    if !matches!(token, Tok::Name { .. }) {
        return relaxed(range);
    }
    loop {
        (token, range) = rest.next()?;
        match token {
            Tok::Dot => {
                let (name, name_range) = rest.next()?;
                if !matches!(name, Tok::Name { .. }) {
                    return relaxed(name_range);
                }
            }
            _ => break,
        }
    }
    // Optional single call: '(' ... balanced ... ')'.
    if matches!(token, Tok::Lpar) {
        let mut depth = 1_u32;
        loop {
            let (inner, _) = rest.next()?;
            match inner {
                Tok::Lpar | Tok::Lsqb | Tok::Lbrace => depth += 1,
                Tok::Rpar | Tok::Rsqb | Tok::Rbrace => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
        (token, range) = rest.next()?;
    }
    if matches!(token, Tok::Newline) {
        None
    } else {
        relaxed(range)
    }
}

// ---------------------------------------------------------------------------
// f-string raw-text scanning (the token carries the raw inner text).
// ---------------------------------------------------------------------------

/// What an f-string's raw inner text revealed.
#[derive(Debug, Default, Clone, Copy)]
struct FStringFindings {
    /// A self-documenting `=` expression (`{x=}`, 3.8).
    self_documenting: bool,
    /// A backslash, newline, or `#` inside an expression part (PEP 701,
    /// 3.12; the bundled parser accepts e.g. `f"{'\n'}"`).
    modern_expression: bool,
}

/// Scans an f-string's raw inner text (UTF-8 bytes; all significant
/// characters are ASCII, so byte scanning is exact). Returns `None` when
/// the text does not scan cleanly — parsed code always should, and on any
/// surprise the honest answer is silence, not a guessed violation.
fn scan_fstring(bytes: &[u8]) -> Option<FStringFindings> {
    let mut findings = FStringFindings::default();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'{' if bytes.get(index + 1) == Some(&b'{') => index += 2,
            b'{' => {
                index += 1;
                scan_replacement_field(bytes, &mut index, &mut findings)?;
            }
            b'}' if bytes.get(index + 1) == Some(&b'}') => index += 2,
            _ => index += 1,
        }
    }
    Some(findings)
}

/// Scans one `{...}` replacement field starting just after its `{`,
/// leaving `index` just past the matching `}`. Recurses for nested
/// fields in format specs and for nested f-string literals inside the
/// expression part.
#[allow(
    clippy::too_many_lines,
    reason = "one coherent state machine; the states are commented inline"
)]
fn scan_replacement_field(
    bytes: &[u8],
    index: &mut usize,
    findings: &mut FStringFindings,
) -> Option<()> {
    let mut depth = 0_u32; // brackets opened inside the expression
    // Expression part.
    while *index < bytes.len() {
        let byte = bytes[*index];
        match byte {
            // PEP 701: pre-3.12 the expression part cannot contain a
            // backslash (even inside a nested string literal), a newline,
            // or a `#` comment.
            b'\\' => {
                findings.modern_expression = true;
                *index += 2; // the backslash escapes the next byte
            }
            b'\n' | b'\r' => {
                findings.modern_expression = true;
                *index += 1;
            }
            b'#' => {
                findings.modern_expression = true;
                // A comment runs to the end of the line.
                while *index < bytes.len() && bytes[*index] != b'\n' {
                    *index += 1;
                }
            }
            b'\'' | b'"' => scan_nested_string(bytes, index, findings)?,
            b'(' | b'[' | b'{' => {
                depth += 1;
                *index += 1;
            }
            b')' | b']' => {
                depth = depth.checked_sub(1)?;
                *index += 1;
            }
            b'}' if depth == 0 => {
                *index += 1;
                return Some(());
            }
            b'}' => {
                depth -= 1;
                *index += 1;
            }
            b':' if depth == 0 => {
                *index += 1;
                return scan_format_spec(bytes, index, findings);
            }
            b'!' if depth == 0 => {
                if bytes.get(*index + 1) == Some(&b'=') {
                    *index += 2; // `!=` comparison, still in the expression
                } else {
                    // Conversion: skip to the spec or the closing brace.
                    *index += 1;
                    return scan_after_conversion(bytes, index, findings);
                }
            }
            b'=' if depth == 0 => {
                if bytes.get(*index + 1) == Some(&b'=') {
                    *index += 2; // `==` comparison
                } else if matches!(
                    index.checked_sub(1).map(|prev| bytes[prev]),
                    Some(b'<' | b'>' | b'=' | b'!')
                ) {
                    *index += 1; // second half of `<=` / `>=`
                } else {
                    // Candidate self-documenting `=`: valid parses allow
                    // only whitespace before `}`, `!conv`, or `:spec`.
                    let mut probe = *index + 1;
                    while bytes.get(probe).is_some_and(u8::is_ascii_whitespace) {
                        probe += 1;
                    }
                    match bytes.get(probe) {
                        Some(b'}' | b'!' | b':') => {
                            findings.self_documenting = true;
                            *index = probe;
                        }
                        // Anything else cannot come out of a successful
                        // 3.12 parse; refuse to guess.
                        _ => return None,
                    }
                }
            }
            _ => *index += 1,
        }
    }
    None // unterminated field
}

/// Skips a conversion (`!r` and friends) up to the spec or closing brace.
fn scan_after_conversion(
    bytes: &[u8],
    index: &mut usize,
    findings: &mut FStringFindings,
) -> Option<()> {
    while *index < bytes.len() {
        match bytes[*index] {
            b'}' => {
                *index += 1;
                return Some(());
            }
            b':' => {
                *index += 1;
                return scan_format_spec(bytes, index, findings);
            }
            _ => *index += 1,
        }
    }
    None
}

/// Scans a format spec: literal text (backslashes, newlines, and `#` are
/// literal here and legal pre-3.12) plus nested replacement fields.
fn scan_format_spec(bytes: &[u8], index: &mut usize, findings: &mut FStringFindings) -> Option<()> {
    while *index < bytes.len() {
        match bytes[*index] {
            b'}' => {
                *index += 1;
                return Some(());
            }
            b'{' => {
                *index += 1;
                scan_replacement_field(bytes, index, findings)?;
            }
            _ => *index += 1,
        }
    }
    None
}

/// Scans a string literal nested inside an f-string expression part; when
/// the nested literal is itself an f-string (pre-3.12 requires different
/// quotes, which the bundled parser enforces), its contents are scanned
/// recursively so `f"{f'{y=}'}"` is still found.
fn scan_nested_string(
    bytes: &[u8],
    index: &mut usize,
    findings: &mut FStringFindings,
) -> Option<()> {
    let quote = bytes[*index];
    // Prefix letters directly before the quote; only a valid f-prefix
    // (`f`, `F`, `rf`, `fR`, ...) marks a nested f-string — an adjacent
    // keyword such as `else'x'` or `if'x'` must not (its `f` is not a
    // prefix).
    let mut prefix_start = *index;
    while prefix_start > 0 && bytes[prefix_start - 1].is_ascii_alphabetic() {
        prefix_start -= 1;
    }
    let prefix = &bytes[prefix_start..*index];
    let is_fstring = prefix.len() <= 2
        && prefix
            .iter()
            .all(|byte| matches!(byte, b'f' | b'F' | b'r' | b'R'))
        && prefix.iter().any(|byte| matches!(byte, b'f' | b'F'));
    let triple = bytes.get(*index + 1) == Some(&quote) && bytes.get(*index + 2) == Some(&quote);
    let quote_len = if triple { 3 } else { 1 };
    let content_start = *index + quote_len;
    let mut cursor = content_start;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => {
                // PEP 701: a backslash anywhere in the expression part —
                // nested literals included — needs 3.12.
                findings.modern_expression = true;
                cursor += 2;
            }
            b'\n' | b'\r' => {
                findings.modern_expression = true;
                cursor += 1;
            }
            byte if byte == quote => {
                if !triple
                    || (bytes.get(cursor + 1) == Some(&quote)
                        && bytes.get(cursor + 2) == Some(&quote))
                {
                    if is_fstring {
                        let inner = &bytes[content_start..cursor];
                        let nested = scan_fstring(inner)?;
                        findings.self_documenting |= nested.self_documenting;
                        findings.modern_expression |= nested.modern_expression;
                    }
                    *index = cursor + quote_len;
                    return Some(());
                }
                cursor += 1;
            }
            _ => cursor += 1,
        }
    }
    None // unterminated nested string
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
        violations(&lex_tokens(text), &parse(text), target)
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

    /// Oracle: `ast.parse(feature_version=(3, 6))` rejects every
    /// `async`/`await` use outside an `async def` and accepts them all at
    /// (3, 7); inside an `async def` — nested plain `def`s included — the
    /// 3.6 tokenizer already recognized the keywords.
    #[test]
    fn async_syntax_outside_async_def_is_gated_below_3_7() {
        for source in [
            "await task\n",
            "async for item in items:\n    pass\n",
            "async with lock:\n    pass\n",
            "def f():\n    return [x async for x in y]\n",
            "def f():\n    return (x async for x in y)\n",
            "def f():\n    return {x async for x in y}\n",
            "def f():\n    return {x: 1 async for x in y}\n",
            "def f():\n    return await task\n",
        ] {
            assert_eq!(
                found(source, (3, 6)),
                [Feature::AsyncOutsideAsyncFunction],
                "{source:?} must be gated at 3.6"
            );
            assert_eq!(found(source, (3, 7)), [], "{source:?} is fine at 3.7");
        }
        for source in [
            "async def f():\n    await task\n",
            "async def f():\n    async with lock:\n        pass\n",
            "async def f():\n    return [x async for x in y]\n",
            // The 3.6 tokenizer keeps the keywords inside nested defs.
            "async def f():\n    def g():\n        return await x\n",
            "async def f():\n    def g():\n        return [i async for i in a]\n",
        ] {
            assert_eq!(found(source, (3, 6)), [], "{source:?} parses on 3.6");
        }
    }

    /// The construct the review flagged as AST-invisible: `f"{x=}"`
    /// desugars to exactly the same AST as `f"x={x!r}"`, so the raw token
    /// text decides.
    #[test]
    fn fstring_self_documenting_equals_is_gated_below_3_8() {
        for source in [
            "print(f\"{x=}\")\n",
            "print(f\"{x = }\")\n",
            "print(f\"{x=:>10}\")\n",
            "print(f\"{x=!r}\")\n",
            "print(f\"{f(1)=}\")\n",
            // Nested f-string carrying the `=` (different quotes, pre-3.12
            // legal nesting).
            "print(f\"{f'{y=}'}\")\n",
            // Nested replacement field inside a format spec.
            "print(f\"{value:{width=}}\")\n",
        ] {
            assert_eq!(
                found(source, (3, 7)),
                [Feature::FStringSelfDocumenting],
                "{source:?} must be gated at 3.7"
            );
            assert_eq!(found(source, (3, 8)), [], "{source:?} is fine at 3.8");
        }
        // The AST twin and every other benign `=` stay silent.
        for source in [
            "print(f\"x={x!r}\")\n",
            "print(f\"{x == y}\")\n",
            "print(f\"{x != y}\")\n",
            "print(f\"{x <= y}\")\n",
            "print(f\"{x >= y}\")\n",
            "print(f\"{f(a=1)}\")\n",
            "print(f\"{'='}\")\n",
            "print(f\"{d['=']}\")\n",
            "print(f\"= {x} =\")\n",
        ] {
            assert_eq!(found(source, (3, 7)), [], "{source:?} has no debug `=`");
        }
    }

    /// PEP 701 relaxations the bundled parser accepts: a backslash,
    /// newline, or `#` inside the expression part needs Python 3.12.
    /// Quote reuse (`f"{"x"}"`) is a bundled parse error and never
    /// reaches the gate.
    #[test]
    fn modern_fstring_expressions_are_gated_below_3_12() {
        for source in [
            "print(f\"{'\\n'}\")\n",
            "print(f\"\"\"{1 +\n1}\"\"\")\n",
            "print(f\"\"\"{x  # why\n}\"\"\")\n",
        ] {
            assert_eq!(
                found(source, (3, 11)),
                [Feature::ModernFStringExpression],
                "{source:?} must be gated at 3.11"
            );
            assert_eq!(found(source, (3, 12)), [], "{source:?} is fine at 3.12");
        }
        for source in [
            // Backslashes and newlines in the literal or spec parts are
            // pre-3.12 legal.
            "print(f\"\\n{x}\")\n",
            "print(f\"{x:\\n}\")\n",
            "print(f\"\"\"line\n{x}\n\"\"\")\n",
            "print(f\"{'#'}\")\n",
            "print(f\"# {x}\")\n",
        ] {
            assert_eq!(found(source, (3, 11)), [], "{source:?} parses pre-3.12");
        }
    }

    /// PEP 614: anything beyond `@dotted.name` / `@dotted.name(args)`.
    #[test]
    fn relaxed_decorators_are_gated_below_3_9() {
        for source in [
            "@buttons[0].clicked\ndef f():\n    pass\n",
            "@(deco)\ndef f():\n    pass\n",
            "@deco(1)(2)\ndef f():\n    pass\n",
            "@deco or fallback\ndef f():\n    pass\n",
            "# lead-in comment\n@registry[\"key\"]\nclass C:\n    pass\n",
            "class C:\n    @props.setter\n    @registry[0]\n    def f(self):\n        pass\n",
        ] {
            assert_eq!(
                found(source, (3, 8)),
                [Feature::RelaxedDecorator],
                "{source:?} must be gated at 3.8"
            );
            assert_eq!(found(source, (3, 9)), [], "{source:?} is fine at 3.9");
        }
        for source in [
            "@deco\ndef f():\n    pass\n",
            "@mod.sub.deco\nasync def f():\n    pass\n",
            "@deco(arg, kw=1)  # trailing comment\ndef f():\n    pass\n",
            "@deco(\n    arg,\n)\ndef f():\n    pass\n",
            // Binary `@` (matmul) is not a decorator, even at a visual
            // line start inside brackets or after a backslash join.
            "z = (x\n@ y)\n",
            "z = x \\\n@ y\n",
        ] {
            assert_eq!(found(source, (3, 8)), [], "{source:?} is pre-3.9 valid");
        }
    }

    /// `as` inside a parenthesized `with` header cannot be the pre-3.9
    /// single-expression reading; plain tuple grouping can, and stays
    /// silent on every version.
    #[test]
    fn parenthesized_context_managers_with_as_are_gated_below_3_9() {
        for source in [
            "with (open(a) as f, open(b) as g):\n    pass\n",
            "with (open(a) as f):\n    pass\n",
            "with (\n    open(a) as f,\n    open(b),\n):\n    pass\n",
            "async def f():\n    async with (open(a) as f, open(b) as g):\n        pass\n",
        ] {
            assert!(
                found(source, (3, 8)).contains(&Feature::ParenthesizedContextManagers),
                "{source:?} must be gated at 3.8"
            );
            assert!(
                !found(source, (3, 9)).contains(&Feature::ParenthesizedContextManagers),
                "{source:?} is fine at 3.9"
            );
        }
        for source in [
            "with (a, b):\n    pass\n",   // tuple context manager, parses everywhere
            "with (a) as b:\n    pass\n", // `as` outside the parens
            "with open(a) as f, open(b) as g:\n    pass\n",
            "with a as (b, c):\n    pass\n", // parenthesized *target*
        ] {
            assert_eq!(found(source, (3, 8)), [], "{source:?} is pre-3.9 valid");
        }
    }

    /// PEP 646: `*` unpacking directly in a subscript or in a `*args`
    /// annotation.
    #[test]
    fn pep646_unpacking_is_gated_below_3_11() {
        for source in [
            "x = matrix[*index]\n",
            "x = matrix[a, *rest]\n",
            "def f(*args: *Ts):\n    pass\n",
        ] {
            assert_eq!(
                found(source, (3, 10)),
                [Feature::SubscriptUnpack],
                "{source:?} must be gated at 3.10"
            );
            assert_eq!(found(source, (3, 11)), [], "{source:?} is fine at 3.11");
        }
        for source in [
            "a, *rest = items\n",  // starred assignment target (3.0)
            "merged = [*a, *b]\n", // display unpacking (3.5, below floor)
            "f(*args)\n",
            "x = matrix[[*a]]\n", // starred inside a nested display
        ] {
            assert_eq!(found(source, (3, 10)), [], "{source:?} predates 3.11");
        }
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
    fn violations_are_reported_in_source_order() {
        let source = "with (a as b):\n    x = q[*i]\nmatch x:\n    case 1:\n        pass\n";
        assert_eq!(
            found(source, (3, 8)),
            [
                Feature::ParenthesizedContextManagers,
                Feature::SubscriptUnpack,
                Feature::MatchStatement,
            ]
        );
    }

    #[test]
    fn ordinary_modern_free_code_is_clean_at_the_floor() {
        let source = "\
from manim import *


class Demo(Scene):
    def construct(self):
        square = Square()
        title = f\"size {square.width:>6.2f}\"
        with open(\"x\") as handle:
            data = handle.read()
        self.play(FadeIn(square))
";
        assert_eq!(found(source, (3, 6)), []);
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

    #[test]
    fn gate_spans_token_detected_constructs_on_the_construct() {
        let mut sources = SourceManager::new("/project");
        sources.load_bytes(
            Path::new("/project/scene.py"),
            b"name = \"x\"\nvalue = f\"{name=}\"\n",
        );
        let file = &sources.files()[0];
        let diagnostics = gate(file, "3.7");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(
            diagnostics[0].message,
            "f-string self-documenting `=` expression requires Python 3.8 \
             but target-python is 3.7"
        );
        assert_eq!(diagnostics[0].primary_span.start.line, 2);
        assert_eq!(diagnostics[0].primary_span.start.column, 9);
        assert!(gate(file, "3.8").is_empty());
    }
}
