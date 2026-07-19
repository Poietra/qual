//! Updater callbacks (DESIGN §3.3): signature summaries and the
//! name-`dt` time-based convention, updater registration / removal with
//! identity matching (MLC125), the conservative updater-body dataflow
//! classifier (MLC112 / MLP218 / MLD301), and callback return facts
//! (MLC123).

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::ast::{self, Ranged};

use crate::frontend::index::{CallableSignature, ParamKind};
use crate::knowledge::SymbolKind;
use crate::semantic::state::{CallbackRef, SignatureSummary, UpdaterFact, WriteChannel};
use crate::semantic::values::{AllocationSite, ObjectId, Truth};
use crate::source::FileId;

use super::dispatch::{Ctx, PURE_BUILTINS};
use super::exec::{ExecState, Machine, OpKind};
use super::heap_ops::mutator_channels;
use super::{
    ReturnFact, UpdaterBodyFact, UpdaterHost, UpdaterRegistration, UpdaterRemoval,
    UpdaterTargetRead,
};

// ---------------------------------------------------------------------------
// Signature summaries (DESIGN §3.3).
// ---------------------------------------------------------------------------

pub(super) fn signature_from_args(args: &ast::Arguments) -> CallableSignature {
    let mut params = Vec::new();
    for arg in &args.posonlyargs {
        params.push(crate::frontend::index::ParamFact {
            name: arg.def.arg.to_string(),
            kind: ParamKind::PositionalOnly,
            has_default: arg.default.is_some(),
        });
    }
    for arg in &args.args {
        params.push(crate::frontend::index::ParamFact {
            name: arg.def.arg.to_string(),
            kind: ParamKind::PositionalOrKeyword,
            has_default: arg.default.is_some(),
        });
    }
    if let Some(vararg) = &args.vararg {
        params.push(crate::frontend::index::ParamFact {
            name: vararg.arg.to_string(),
            kind: ParamKind::VarArgs,
            has_default: false,
        });
    }
    for arg in &args.kwonlyargs {
        params.push(crate::frontend::index::ParamFact {
            name: arg.def.arg.to_string(),
            kind: ParamKind::KeywordOnly,
            has_default: arg.default.is_some(),
        });
    }
    if let Some(kwarg) = &args.kwarg {
        params.push(crate::frontend::index::ParamFact {
            name: kwarg.arg.to_string(),
            kind: ParamKind::KwArgs,
            has_default: false,
        });
    }
    CallableSignature { params }
}

/// Whether Manim's positional invocation with `provided` arguments binds.
fn binds_positionally(signature: &CallableSignature, provided: usize) -> Truth {
    let mut max_positional = 0usize;
    let mut required_positional = 0usize;
    let mut has_varargs = false;
    let mut required_keyword_only = false;
    for param in &signature.params {
        match param.kind {
            ParamKind::PositionalOnly | ParamKind::PositionalOrKeyword => {
                max_positional += 1;
                if !param.has_default {
                    required_positional += 1;
                }
            }
            ParamKind::VarArgs => has_varargs = true,
            ParamKind::KeywordOnly => {
                if !param.has_default {
                    required_keyword_only = true;
                }
            }
            ParamKind::KwArgs => {}
        }
    }
    if required_keyword_only {
        return Truth::No;
    }
    let enough = provided >= required_positional;
    let not_too_many = has_varargs || provided <= max_positional;
    Truth::from(enough && not_too_many)
}

/// Builds the updater fact for a callback (DESIGN §3.3: the *name* `dt`
/// decides the time-based call, not the arity).
pub(super) fn updater_fact(
    callback: CallbackRef,
    signature: Option<&CallableSignature>,
    scene_level: bool,
) -> UpdaterFact {
    let summary = signature.map_or_else(SignatureSummary::unknown, |signature| {
        let has_dt = signature.params.iter().any(|param| param.name == "dt");
        let positional = signature
            .params
            .iter()
            .filter(|param| {
                matches!(
                    param.kind,
                    ParamKind::PositionalOnly | ParamKind::PositionalOrKeyword
                )
            })
            .count();
        let provided = if scene_level {
            1
        } else if has_dt {
            2
        } else {
            1
        };
        SignatureSummary {
            positional_params: u8::try_from(positional).ok(),
            has_dt_named_param: Truth::from(has_dt),
            has_var_positional: Truth::from(
                signature
                    .params
                    .iter()
                    .any(|param| param.kind == ParamKind::VarArgs),
            ),
            binds_positionally: binds_positionally(signature, provided),
        }
    });
    let time_based = if scene_level {
        // Scene updaters always receive `(dt)` and always make waits
        // dynamic (DESIGN §3.3).
        Truth::Yes
    } else {
        summary.has_dt_named_param
    };
    UpdaterFact {
        callback,
        signature: summary,
        time_based,
    }
}

impl Machine<'_, '_> {
    pub(super) fn register_updater(
        &mut self,
        host: Option<&ObjectId>,
        fact: UpdaterFact,
        site: AllocationSite,
        state: &mut ExecState,
    ) {
        match host {
            Some(target) => {
                if let Some(object) = state.heap.object_mut(target) {
                    object.updaters.insert(fact.clone());
                }
            }
            None => {
                if let Some(scene) = self.scene_state_mut(state) {
                    scene.scene_updaters.insert(fact.clone());
                }
            }
        }
        if self.emit {
            self.sink.updaters.push(UpdaterRegistration {
                site,
                host: host.map_or(UpdaterHost::Scene, |id| UpdaterHost::Mobject(id.clone())),
                fact: fact.clone(),
                // Body dataflow is classified after the scene run, once the
                // callback bodies of the whole project are indexed.
                body: UpdaterBodyFact::unknown(),
                certainty: self.certainty(),
            });
        }
        self.record(
            site,
            OpKind::RegisterUpdater {
                target: host.cloned(),
                scene_level: host.is_none(),
                updater: fact,
            },
        );
    }

    pub(super) fn remove_updater(
        &mut self,
        host: Option<&ObjectId>,
        callback: &CallbackRef,
        site: AllocationSite,
        state: &mut ExecState,
    ) {
        let matched = if let Some(target) = host {
            let registered = state
                .heap
                .object(target)
                .map(|object| object.updaters.clone())
                .unwrap_or_default();
            Self::match_and_remove(&registered, callback, |updaters| {
                if let Some(object) = state.heap.object_mut(target) {
                    object.updaters = updaters;
                }
            })
        } else {
            {
                let registered = state
                    .heap
                    .scenes
                    .get(&self.scene_id)
                    .map(|scene| scene.scene_updaters.clone())
                    .unwrap_or_default();
                Self::match_and_remove(&registered, callback, |updaters| {
                    if let Some(scene) = state.heap.scenes.get_mut(&self.scene_id) {
                        scene.scene_updaters = updaters;
                    }
                })
            }
        };
        if self.emit {
            let certainty = self.certainty();
            self.sink.updater_removals.push(UpdaterRemoval {
                site,
                host: host.map_or(UpdaterHost::Scene, |id| UpdaterHost::Mobject(id.clone())),
                callback: callback.clone(),
                matched,
                certainty,
            });
        }
        self.record(
            site,
            OpKind::RemoveUpdater {
                target: host.cloned(),
                scene_level: host.is_none(),
                callback: callback.clone(),
            },
        );
    }

    fn match_and_remove(
        registered: &BTreeSet<UpdaterFact>,
        callback: &CallbackRef,
        write_back: impl FnOnce(BTreeSet<UpdaterFact>),
    ) -> Truth {
        if matches!(callback, CallbackRef::Unknown) {
            // Unknown identity: it may match anything; leave the set.
            return Truth::Maybe;
        }
        let matches: Vec<&UpdaterFact> = registered
            .iter()
            .filter(|fact| &fact.callback == callback)
            .collect();
        if matches.is_empty() {
            // A distinct identity (e.g. a fresh lambda) definitely does
            // not match any registered updater.
            return Truth::No;
        }
        let remaining: BTreeSet<UpdaterFact> = registered
            .iter()
            .filter(|fact| &fact.callback != callback)
            .cloned()
            .collect();
        write_back(remaining);
        Truth::Yes
    }
}

// ---------------------------------------------------------------------------
// Updater-body dataflow classification (MLC112 / MLP218 / MLD301).
// ---------------------------------------------------------------------------

/// Methods on the updater parameter allowed by `pure_affine_on_target`.
fn is_affine_or_setter(method: &str) -> bool {
    matches!(method, "shift" | "rotate" | "scale" | "move_to") || method.starts_with("set_")
}

/// Curated frame-varying read sources, by canonical candidate id.
/// `Yes`-evidence is reserved for provable reads (MLC112 fires on `Yes`);
/// everything else stays `Maybe` at most.
fn frame_varying_candidate(candidate: &str) -> bool {
    (candidate.starts_with("manim.mobject.value_tracker.") && candidate.ends_with(".get_value"))
        || (candidate.starts_with("random.") && candidate != "random.seed")
        || candidate.starts_with("numpy.random.")
        || matches!(
            candidate,
            "time.time"
                | "time.monotonic"
                | "time.perf_counter"
                | "time.time_ns"
                | "time.monotonic_ns"
                | "time.perf_counter_ns"
        )
}

/// Candidates that are provably pure, deterministic computations.
fn pure_deterministic_candidate(candidate: &str) -> bool {
    (candidate.starts_with("numpy.") && !candidate.starts_with("numpy.random."))
        || candidate.starts_with("math.")
}

/// Accumulates occurrence evidence: `Yes` sticks, any `Maybe` degrades a
/// `No`, and `No` + `No` stays `No`.
fn bump(slot: &mut Truth, evidence: Truth) {
    *slot = match (*slot, evidence) {
        (Truth::Yes, _) | (_, Truth::Yes) => Truth::Yes,
        (Truth::No, Truth::No) => Truth::No,
        _ => Truth::Maybe,
    };
}

/// The root name of a (possibly dotted / subscripted) expression.
fn root_name(expr: &ast::Expr) -> Option<&str> {
    match expr {
        ast::Expr::Name(name) => Some(name.id.as_str()),
        ast::Expr::Attribute(attribute) => root_name(&attribute.value),
        ast::Expr::Subscript(subscript) => root_name(&subscript.value),
        ast::Expr::Starred(starred) => root_name(&starred.value),
        _ => None,
    }
}

/// The body being classified.
enum BodyRef<'a> {
    /// A `def` body.
    Stmts(&'a [ast::Stmt]),
    /// A lambda body (one expression, evaluated unconditionally).
    Expr(&'a ast::Expr),
}

/// Syntactic, conservative classifier over one updater callback body.
struct BodyClassifier<'a, 'b> {
    ctx: &'b Ctx<'a>,
    file: FileId,
    /// The updater's mobject parameter name (`None` for scene updaters or
    /// while shadowed by a nested scope).
    target: Option<String>,
    /// The frame-delta parameter name (`None` while shadowed).
    dt: Option<String>,
    /// Depth inside nested callables defined in the body (their code may
    /// or may not run per frame: evidence degrades to `Maybe`).
    nested: u32,
    /// Depth inside conditionally executed regions (branch arms, loop
    /// bodies, handlers, short-circuit operands): a conditional read is
    /// not a per-frame proof.
    conditional: u32,
    uses_dt: bool,
    reads_frame_varying: Truth,
    mutates_target: Truth,
    write_channels: BTreeSet<WriteChannel>,
    target_reads: Vec<UpdaterTargetRead>,
    channels_known: Truth,
    /// Evidence of an operation outside the pure-affine allowlist.
    disallowed: Truth,
    calls_unknown: Truth,
}

impl BodyClassifier<'_, '_> {
    fn evidence(&self) -> Truth {
        if self.nested > 0 || self.conditional > 0 {
            Truth::Maybe
        } else {
            Truth::Yes
        }
    }

    fn walk_stmts(&mut self, stmts: &[ast::Stmt]) {
        for stmt in stmts {
            self.walk_stmt(stmt);
        }
    }

    fn conditional_stmts(&mut self, stmts: &[ast::Stmt]) {
        self.conditional += 1;
        self.walk_stmts(stmts);
        self.conditional -= 1;
    }

    fn conditional_expr(&mut self, expr: &ast::Expr) {
        self.conditional += 1;
        self.walk_expr(expr);
        self.conditional -= 1;
    }

    /// Enters a nested callable scope, shadowing re-bound tracked names.
    fn nested_scope(&mut self, params: &ast::Arguments, walk: impl FnOnce(&mut Self)) {
        let mut bound: BTreeSet<String> = BTreeSet::new();
        for arg in params
            .posonlyargs
            .iter()
            .chain(&params.args)
            .chain(&params.kwonlyargs)
        {
            bound.insert(arg.def.arg.to_string());
        }
        for arg in [&params.vararg, &params.kwarg].into_iter().flatten() {
            bound.insert(arg.arg.to_string());
        }
        let saved_dt = self.dt.clone();
        let saved_target = self.target.clone();
        if self.dt.as_ref().is_some_and(|name| bound.contains(name)) {
            self.dt = None;
        }
        if self
            .target
            .as_ref()
            .is_some_and(|name| bound.contains(name))
        {
            self.target = None;
        }
        self.nested += 1;
        walk(self);
        self.nested -= 1;
        self.dt = saved_dt;
        self.target = saved_target;
    }

    /// A write target of an assignment / deletion.
    fn classify_store_target(&mut self, target: &ast::Expr) {
        match target {
            // Local rebinds are pure.
            ast::Expr::Name(_) => {}
            ast::Expr::Tuple(inner) => {
                for element in &inner.elts {
                    self.classify_store_target(element);
                }
            }
            ast::Expr::List(inner) => {
                for element in &inner.elts {
                    self.classify_store_target(element);
                }
            }
            ast::Expr::Starred(inner) => self.classify_store_target(&inner.value),
            _ => {
                // Attribute / subscript writes: a raw mutation of whatever
                // the root binding refers to (e.g. `mob.points = ...`).
                let evidence = self.evidence();
                if root_name(target).is_some_and(|root| self.target.as_deref() == Some(root)) {
                    bump(&mut self.mutates_target, evidence);
                    self.channels_known = Truth::Maybe;
                }
                bump(&mut self.disallowed, evidence);
                self.walk_expr(target);
            }
        }
    }

    #[allow(clippy::too_many_lines, reason = "one arm per statement kind")]
    fn walk_stmt(&mut self, stmt: &ast::Stmt) {
        match stmt {
            ast::Stmt::Expr(inner) => self.walk_expr(&inner.value),
            ast::Stmt::Return(inner) => {
                if let Some(value) = &inner.value {
                    self.walk_expr(value);
                }
            }
            ast::Stmt::Assign(inner) => {
                self.walk_expr(&inner.value);
                for target in &inner.targets {
                    self.classify_store_target(target);
                }
            }
            ast::Stmt::AugAssign(inner) => {
                self.walk_expr(&inner.value);
                self.classify_store_target(&inner.target);
            }
            ast::Stmt::AnnAssign(inner) => {
                if let Some(value) = &inner.value {
                    self.walk_expr(value);
                    self.classify_store_target(&inner.target);
                }
            }
            ast::Stmt::If(inner) => {
                self.walk_expr(&inner.test);
                self.conditional_stmts(&inner.body);
                self.conditional_stmts(&inner.orelse);
            }
            ast::Stmt::While(inner) => {
                self.walk_expr(&inner.test);
                self.conditional_stmts(&inner.body);
                self.conditional_stmts(&inner.orelse);
            }
            ast::Stmt::For(inner) => {
                self.walk_expr(&inner.iter);
                self.conditional_stmts(&inner.body);
                self.conditional_stmts(&inner.orelse);
            }
            ast::Stmt::AsyncFor(inner) => {
                self.walk_expr(&inner.iter);
                self.conditional_stmts(&inner.body);
                self.conditional_stmts(&inner.orelse);
            }
            ast::Stmt::With(inner) => {
                for item in &inner.items {
                    self.walk_expr(&item.context_expr);
                }
                self.walk_stmts(&inner.body);
            }
            ast::Stmt::AsyncWith(inner) => {
                for item in &inner.items {
                    self.walk_expr(&item.context_expr);
                }
                self.walk_stmts(&inner.body);
            }
            ast::Stmt::Try(inner) => {
                self.conditional_stmts(&inner.body);
                for handler in &inner.handlers {
                    let ast::ExceptHandler::ExceptHandler(handler) = handler;
                    if let Some(kind) = &handler.type_ {
                        self.walk_expr(kind);
                    }
                    self.conditional_stmts(&handler.body);
                }
                self.conditional_stmts(&inner.orelse);
                self.walk_stmts(&inner.finalbody);
            }
            ast::Stmt::TryStar(inner) => {
                self.conditional_stmts(&inner.body);
                for handler in &inner.handlers {
                    let ast::ExceptHandler::ExceptHandler(handler) = handler;
                    if let Some(kind) = &handler.type_ {
                        self.walk_expr(kind);
                    }
                    self.conditional_stmts(&handler.body);
                }
                self.conditional_stmts(&inner.orelse);
                self.walk_stmts(&inner.finalbody);
            }
            ast::Stmt::Match(inner) => {
                self.walk_expr(&inner.subject);
                for case in &inner.cases {
                    if let Some(guard) = &case.guard {
                        self.conditional_expr(guard);
                    }
                    self.conditional_stmts(&case.body);
                }
            }
            ast::Stmt::Raise(inner) => {
                for part in [&inner.exc, &inner.cause].into_iter().flatten() {
                    self.walk_expr(part);
                }
            }
            ast::Stmt::Assert(inner) => {
                self.walk_expr(&inner.test);
                if let Some(message) = &inner.msg {
                    self.walk_expr(message);
                }
            }
            ast::Stmt::Delete(inner) => {
                for target in &inner.targets {
                    self.classify_store_target(target);
                }
            }
            ast::Stmt::FunctionDef(def) => {
                collect_defaults_walk(self, &def.args);
                self.nested_scope(&def.args, |classifier| {
                    classifier.walk_stmts(&def.body);
                });
            }
            ast::Stmt::AsyncFunctionDef(def) => {
                collect_defaults_walk(self, &def.args);
                self.nested_scope(&def.args, |classifier| {
                    classifier.walk_stmts(&def.body);
                });
            }
            ast::Stmt::ClassDef(def) => {
                // A class body executes at definition time; treat it as a
                // nested (maybe-running) region.
                self.nested += 1;
                self.walk_stmts(&def.body);
                self.nested -= 1;
            }
            ast::Stmt::Global(_) | ast::Stmt::Nonlocal(_) => {
                // Declared intent to write enclosing-scope state.
                bump(&mut self.disallowed, Truth::Maybe);
            }
            ast::Stmt::Import(_) | ast::Stmt::ImportFrom(_) => {
                bump(&mut self.disallowed, Truth::Maybe);
            }
            ast::Stmt::TypeAlias(_)
            | ast::Stmt::Pass(_)
            | ast::Stmt::Break(_)
            | ast::Stmt::Continue(_) => {}
        }
    }

    #[allow(clippy::too_many_lines, reason = "one arm per expression kind")]
    fn walk_expr(&mut self, expr: &ast::Expr) {
        match expr {
            ast::Expr::Name(name) => {
                if self.dt.as_deref() == Some(name.id.as_str()) {
                    self.uses_dt = true;
                }
            }
            ast::Expr::Call(call) => self.classify_call(call),
            ast::Expr::Lambda(lambda) => {
                collect_defaults_walk(self, &lambda.args);
                self.nested_scope(&lambda.args, |classifier| {
                    classifier.walk_expr(&lambda.body);
                });
            }
            ast::Expr::BoolOp(inner) => {
                let mut values = inner.values.iter();
                if let Some(first) = values.next() {
                    self.walk_expr(first);
                }
                for value in values {
                    self.conditional_expr(value);
                }
            }
            ast::Expr::NamedExpr(inner) => {
                self.walk_expr(&inner.value);
            }
            ast::Expr::BinOp(inner) => {
                self.walk_expr(&inner.left);
                self.walk_expr(&inner.right);
            }
            ast::Expr::UnaryOp(inner) => self.walk_expr(&inner.operand),
            ast::Expr::IfExp(inner) => {
                self.walk_expr(&inner.test);
                self.conditional_expr(&inner.body);
                self.conditional_expr(&inner.orelse);
            }
            ast::Expr::Dict(inner) => {
                for key in inner.keys.iter().flatten() {
                    self.walk_expr(key);
                }
                for value in &inner.values {
                    self.walk_expr(value);
                }
            }
            ast::Expr::Set(inner) => {
                for element in &inner.elts {
                    self.walk_expr(element);
                }
            }
            ast::Expr::ListComp(inner) => {
                self.walk_generators(&inner.generators, &[&inner.elt]);
            }
            ast::Expr::SetComp(inner) => {
                self.walk_generators(&inner.generators, &[&inner.elt]);
            }
            ast::Expr::DictComp(inner) => {
                self.walk_generators(&inner.generators, &[&inner.key, &inner.value]);
            }
            ast::Expr::GeneratorExp(inner) => {
                self.walk_generators(&inner.generators, &[&inner.elt]);
            }
            ast::Expr::Await(inner) => {
                bump(&mut self.disallowed, Truth::Maybe);
                self.walk_expr(&inner.value);
            }
            ast::Expr::Yield(inner) => {
                bump(&mut self.disallowed, Truth::Maybe);
                if let Some(value) = &inner.value {
                    self.walk_expr(value);
                }
            }
            ast::Expr::YieldFrom(inner) => {
                bump(&mut self.disallowed, Truth::Maybe);
                self.walk_expr(&inner.value);
            }
            ast::Expr::Compare(inner) => {
                self.walk_expr(&inner.left);
                for comparator in &inner.comparators {
                    self.walk_expr(comparator);
                }
            }
            ast::Expr::FormattedValue(inner) => self.walk_expr(&inner.value),
            ast::Expr::JoinedStr(inner) => {
                for value in &inner.values {
                    self.walk_expr(value);
                }
            }
            ast::Expr::Attribute(inner) => self.walk_expr(&inner.value),
            ast::Expr::Subscript(inner) => {
                self.walk_expr(&inner.value);
                self.walk_expr(&inner.slice);
            }
            ast::Expr::Starred(inner) => self.walk_expr(&inner.value),
            ast::Expr::List(inner) => {
                for element in &inner.elts {
                    self.walk_expr(element);
                }
            }
            ast::Expr::Tuple(inner) => {
                for element in &inner.elts {
                    self.walk_expr(element);
                }
            }
            ast::Expr::Slice(inner) => {
                for part in [&inner.lower, &inner.upper, &inner.step]
                    .into_iter()
                    .flatten()
                {
                    self.walk_expr(part);
                }
            }
            ast::Expr::Constant(_) => {}
        }
    }

    fn walk_generators(&mut self, generators: &[ast::Comprehension], elements: &[&ast::Expr]) {
        // The first iterable evaluates unconditionally; everything else
        // runs per element (0..n times).
        let mut iterables = generators.iter().map(|generator| &generator.iter);
        if let Some(first) = iterables.next() {
            self.walk_expr(first);
        }
        for iterable in iterables {
            self.conditional_expr(iterable);
        }
        // Comprehension targets shadow tracked names inside the element.
        let mut bound: BTreeSet<String> = BTreeSet::new();
        for generator in generators {
            crate::frontend::names::collect_target_names(&generator.target, &mut bound);
        }
        let saved_dt = self.dt.clone();
        let saved_target = self.target.clone();
        if self.dt.as_ref().is_some_and(|name| bound.contains(name)) {
            self.dt = None;
        }
        if self
            .target
            .as_ref()
            .is_some_and(|name| bound.contains(name))
        {
            self.target = None;
        }
        for generator in generators {
            for condition in &generator.ifs {
                self.conditional_expr(condition);
            }
        }
        for element in elements {
            self.conditional_expr(element);
        }
        self.dt = saved_dt;
        self.target = saved_target;
    }

    /// The updater parameter appearing as an argument of a call whose
    /// effects are not modeled: the callee may mutate it.
    fn check_target_escape(&mut self, call: &ast::ExprCall) {
        let Some(target) = self.target.clone() else {
            return;
        };
        let mentions = call
            .args
            .iter()
            .chain(call.keywords.iter().map(|keyword| &keyword.value))
            .any(|argument| root_name(argument) == Some(target.as_str()));
        if mentions {
            bump(&mut self.mutates_target, Truth::Maybe);
            self.channels_known = Truth::Maybe;
            bump(&mut self.disallowed, Truth::Maybe);
        }
    }

    fn walk_call_args(&mut self, call: &ast::ExprCall) {
        for argument in &call.args {
            self.walk_expr(argument);
        }
        for keyword in &call.keywords {
            self.walk_expr(&keyword.value);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one conservative branch per callback call category"
    )]
    fn classify_call(&mut self, call: &ast::ExprCall) {
        let evidence = self.evidence();
        // Method call on the updater's mobject parameter.
        if let ast::Expr::Attribute(attribute) = call.func.as_ref() {
            if let ast::Expr::Name(base) = attribute.value.as_ref() {
                if self.target.as_deref() == Some(base.id.as_str()) {
                    let method = attribute.attr.as_str();
                    let channels = mutator_channels(&format!("target.{method}"));
                    if is_affine_or_setter(method) {
                        bump(&mut self.mutates_target, evidence);
                        if let Some(channels) = channels {
                            if channels.contains(&WriteChannel::Points) {
                                for argument in call
                                    .args
                                    .iter()
                                    .chain(call.keywords.iter().map(|keyword| &keyword.value))
                                {
                                    if let Some(read) = direct_target_read(
                                        self.file,
                                        argument,
                                        self.target.as_deref(),
                                        evidence,
                                    ) {
                                        self.target_reads.push(read);
                                    }
                                }
                            }
                            self.write_channels.extend(channels);
                        } else {
                            self.channels_known = Truth::Maybe;
                        }
                    } else if method.starts_with("get_") || method == "copy" {
                        // Curated read convention: `get_*` / `copy` do not
                        // mutate the receiver.
                    } else if mutator_channels(&format!("target.{method}")).is_some()
                        || matches!(
                            method,
                            "become"
                                | "add"
                                | "remove"
                                | "add_updater"
                                | "remove_updater"
                                | "clear_updaters"
                                | "generate_target"
                                | "save_state"
                                | "restore"
                        )
                    {
                        // A curated mutator outside the affine allowlist.
                        bump(&mut self.mutates_target, evidence);
                        if let Some(channels) = channels {
                            self.write_channels.extend(channels);
                        } else if matches!(method, "add" | "remove") {
                            self.write_channels.insert(WriteChannel::Membership);
                        } else {
                            self.channels_known = Truth::Maybe;
                        }
                        bump(&mut self.disallowed, evidence);
                    } else {
                        // Unrecognized method on the target parameter.
                        bump(&mut self.mutates_target, Truth::Maybe);
                        self.channels_known = Truth::Maybe;
                        bump(&mut self.calls_unknown, evidence);
                        bump(&mut self.disallowed, Truth::Maybe);
                    }
                    self.walk_call_args(call);
                    return;
                }
            }
        }
        // A callee the frontend resolved to candidates.
        let candidates = self
            .ctx
            .fact(self.file, call.range())
            .map(|fact| fact.candidates.clone())
            .filter(|candidates| !candidates.is_empty());
        if let Some(candidates) = candidates {
            if candidates
                .iter()
                .all(|candidate| frame_varying_candidate(candidate))
            {
                bump(&mut self.reads_frame_varying, evidence);
            } else if candidates
                .iter()
                .any(|candidate| frame_varying_candidate(candidate))
            {
                bump(&mut self.reads_frame_varying, Truth::Maybe);
                bump(&mut self.disallowed, Truth::Maybe);
            } else if candidates
                .iter()
                .all(|candidate| pure_deterministic_candidate(candidate))
            {
                // Pure computation: no reads, no effects.
            } else {
                // Resolvable, but its effects are not modeled here: not an
                // unknown call, yet also not provably frame-invariant or
                // pure-affine.
                bump(&mut self.reads_frame_varying, Truth::Maybe);
                bump(&mut self.disallowed, Truth::Maybe);
            }
            self.check_target_escape(call);
            self.walk_expr(&call.func);
            self.walk_call_args(call);
            return;
        }
        // No candidates: a builtin or an unresolvable callee.
        if let ast::Expr::Name(name) = call.func.as_ref() {
            let name = name.id.as_str();
            if name == "next" {
                // Iterator advancement reads (and moves) external state.
                bump(&mut self.reads_frame_varying, evidence);
                bump(&mut self.disallowed, Truth::Maybe);
                self.check_target_escape(call);
                self.walk_call_args(call);
                return;
            }
            if PURE_BUILTINS.contains(&name) {
                self.walk_call_args(call);
                return;
            }
        }
        bump(&mut self.calls_unknown, evidence);
        bump(&mut self.disallowed, Truth::Maybe);
        self.check_target_escape(call);
        self.walk_expr(&call.func);
        self.walk_call_args(call);
    }

    fn finish(self, has_dt_param: bool) -> UpdaterBodyFact {
        let uses_dt = if has_dt_param {
            Truth::from(self.uses_dt)
        } else {
            Truth::No
        };
        let calls_unknown = self.calls_unknown;
        // "Any call to an unresolvable function → everything downstream
        // Maybe": an unknown callee may read frame-varying state or reach
        // the target through another alias.
        let degrade = |value: Truth| {
            if calls_unknown == Truth::No || value == Truth::Yes {
                value
            } else {
                Truth::Maybe
            }
        };
        let pure_affine_on_target = match self.disallowed {
            Truth::No => Truth::Yes,
            Truth::Maybe => Truth::Maybe,
            Truth::Yes => Truth::No,
        };
        UpdaterBodyFact {
            uses_dt,
            reads_frame_varying: degrade(self.reads_frame_varying),
            mutates_target: degrade(self.mutates_target),
            write_channels: self.write_channels,
            target_reads: self.target_reads,
            channels_known: if calls_unknown == Truth::No {
                self.channels_known
            } else {
                Truth::Maybe
            },
            pure_affine_on_target,
            calls_unknown,
        }
    }
}

/// A direct geometry read used as the value of a target mutator argument,
/// currently the high-confidence `driver.get_center()` shape. Nested
/// arithmetic, subscripts, and arbitrary helpers stay unknown because the
/// read may not actually determine the written value.
fn direct_target_read(
    file: FileId,
    expr: &ast::Expr,
    target: Option<&str>,
    certainty: Truth,
) -> Option<UpdaterTargetRead> {
    let ast::Expr::Call(call) = expr else {
        return None;
    };
    if !call.args.is_empty() || !call.keywords.is_empty() {
        return None;
    }
    let ast::Expr::Attribute(method) = call.func.as_ref() else {
        return None;
    };
    if method.attr.as_str() != "get_center" {
        return None;
    }
    let binding = callback_object_binding(&method.value)?;
    if target == Some(binding.as_str()) {
        return None;
    }
    Some(UpdaterTargetRead {
        binding,
        method: method.attr.to_string(),
        site: AllocationSite::new(file, call.range()),
        channel: WriteChannel::Points,
        certainty,
    })
}

fn callback_object_binding(expr: &ast::Expr) -> Option<String> {
    match expr {
        ast::Expr::Name(name) => Some(name.id.to_string()),
        ast::Expr::Attribute(attribute) => {
            let ast::Expr::Name(root) = attribute.value.as_ref() else {
                return None;
            };
            (root.id.as_str() == "self").then(|| format!("self.{}", attribute.attr))
        }
        _ => None,
    }
}

/// Walks the default expressions of a nested callable at the *current*
/// scope (defaults evaluate at definition time).
fn collect_defaults_walk(classifier: &mut BodyClassifier<'_, '_>, args: &ast::Arguments) {
    for arg in args
        .posonlyargs
        .iter()
        .chain(&args.args)
        .chain(&args.kwonlyargs)
    {
        if let Some(default) = &arg.default {
            classifier.walk_expr(default);
        }
    }
}

/// Classifies one callback body.
fn classify_body(
    ctx: &Ctx<'_>,
    file: FileId,
    args: &ast::Arguments,
    scene_level: bool,
    body: &BodyRef<'_>,
) -> UpdaterBodyFact {
    let positional: Vec<String> = args
        .posonlyargs
        .iter()
        .chain(&args.args)
        .map(|arg| arg.def.arg.to_string())
        .collect();
    let (target, dt) = if scene_level {
        // Scene updaters always receive exactly `(dt)` (DESIGN §3.3).
        (None, positional.first().cloned())
    } else {
        let dt = args
            .posonlyargs
            .iter()
            .chain(&args.args)
            .chain(&args.kwonlyargs)
            .map(|arg| arg.def.arg.as_str())
            .find(|name| *name == "dt")
            .map(str::to_owned);
        (positional.first().cloned(), dt)
    };
    let has_dt_param = dt.is_some();
    let mut classifier = BodyClassifier {
        ctx,
        file,
        target,
        dt,
        nested: 0,
        conditional: 0,
        uses_dt: false,
        reads_frame_varying: Truth::No,
        mutates_target: Truth::No,
        write_channels: BTreeSet::new(),
        target_reads: Vec::new(),
        channels_known: Truth::Yes,
        disallowed: Truth::No,
        calls_unknown: Truth::No,
    };
    match body {
        BodyRef::Stmts(stmts) => classifier.walk_stmts(stmts),
        BodyRef::Expr(expr) => classifier.walk_expr(expr),
    }
    classifier.finish(has_dt_param)
}

/// Resolves a registered updater callback to its body and classifies it;
/// unresolvable callbacks stay all-`Maybe`.
pub(super) fn classify_updater_callback(
    ctx: &Ctx<'_>,
    lambdas: &BTreeMap<AllocationSite, &ast::ExprLambda>,
    scene_level: bool,
    callback: &CallbackRef,
) -> UpdaterBodyFact {
    match callback {
        CallbackRef::Named(qualified) => match ctx.defs.defs.get(qualified) {
            Some(def) => classify_body(
                ctx,
                def.file,
                def.args,
                scene_level,
                &BodyRef::Stmts(def.body),
            ),
            None => UpdaterBodyFact::unknown(),
        },
        CallbackRef::Lambda(site) => match lambdas.get(site) {
            Some(lambda) => classify_body(
                ctx,
                site.file,
                &lambda.args,
                scene_level,
                &BodyRef::Expr(&lambda.body),
            ),
            None => UpdaterBodyFact::unknown(),
        },
        CallbackRef::Unknown => UpdaterBodyFact::unknown(),
    }
}

// ---------------------------------------------------------------------------
// Lambda return facts (MLC123).
// ---------------------------------------------------------------------------

/// The return fact of one lambda: a lambda always returns its body
/// expression, so bare-return and no-return paths are definite `No`s and
/// only the mobject-ness of the body is classified.
pub(super) fn lambda_return_fact(
    ctx: &Ctx<'_>,
    file: FileId,
    lambda: &ast::ExprLambda,
) -> ReturnFact {
    let param = lambda
        .args
        .posonlyargs
        .iter()
        .chain(&lambda.args.args)
        .next()
        .map(|arg| arg.def.arg.to_string());
    ReturnFact {
        returns_mobject: classify_mobject_expr(ctx, file, param.as_deref(), &lambda.body),
        has_bare_return_path: Truth::No,
        has_no_return_path: Truth::No,
    }
}

/// Whether an expression's value is a mobject, assuming the first
/// parameter (`param`) is one (the `ApplyFunction` callback contract).
fn classify_mobject_expr(
    ctx: &Ctx<'_>,
    file: FileId,
    param: Option<&str>,
    expr: &ast::Expr,
) -> Truth {
    match expr {
        // Literals, displays, strings, and nested lambdas are definitely
        // not mobjects.
        ast::Expr::Constant(_)
        | ast::Expr::JoinedStr(_)
        | ast::Expr::Dict(_)
        | ast::Expr::Set(_)
        | ast::Expr::List(_)
        | ast::Expr::Tuple(_)
        | ast::Expr::ListComp(_)
        | ast::Expr::SetComp(_)
        | ast::Expr::DictComp(_)
        | ast::Expr::GeneratorExp(_)
        | ast::Expr::Lambda(_)
        | ast::Expr::Compare(_)
        | ast::Expr::BoolOp(_) => Truth::No,
        ast::Expr::Name(name) if param == Some(name.id.as_str()) => Truth::Yes,
        ast::Expr::IfExp(inner) => {
            let then_arm = classify_mobject_expr(ctx, file, param, &inner.body);
            let else_arm = classify_mobject_expr(ctx, file, param, &inner.orelse);
            match (then_arm, else_arm) {
                (Truth::Yes, Truth::Yes) => Truth::Yes,
                (Truth::No, _) | (_, Truth::No) => Truth::No,
                _ => Truth::Maybe,
            }
        }
        ast::Expr::Call(call) => {
            let Some(fact) = ctx.fact(file, call.range()) else {
                return Truth::Maybe;
            };
            if fact.candidates.is_empty() {
                return Truth::Maybe;
            }
            let mobject_candidate = |candidate: &str| {
                ctx.index.mobject_classes.contains(candidate)
                    || ctx
                        .knowledge
                        .and_then(|profile| profile.symbol(candidate))
                        .is_some_and(|entry| {
                            matches!(entry.kind, SymbolKind::Mobject | SymbolKind::Vmobject)
                        })
            };
            if fact
                .candidates
                .iter()
                .all(|candidate| mobject_candidate(candidate))
            {
                return Truth::Yes;
            }
            // A single project callable: delegate to its summary fact.
            if fact.candidates.len() == 1 {
                let candidate = fact.candidates.iter().next().expect("len checked");
                if let Some(summary) = ctx.summaries.get(candidate) {
                    return summary.return_fact.returns_mobject;
                }
            }
            let non_mobject_candidate = |candidate: &str| {
                ctx.index.animation_classes.contains(candidate)
                    || ctx
                        .knowledge
                        .and_then(|profile| profile.symbol(candidate))
                        .is_some_and(|entry| {
                            matches!(
                                entry.kind,
                                SymbolKind::Animation | SymbolKind::Scene | SymbolKind::Camera
                            )
                        })
            };
            if fact
                .candidates
                .iter()
                .all(|candidate| non_mobject_candidate(candidate))
            {
                Truth::No
            } else {
                Truth::Maybe
            }
        }
        _ => Truth::Maybe,
    }
}
