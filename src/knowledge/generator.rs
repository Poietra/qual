//! Static candidate generator and drift check for knowledge profiles
//! (DESIGN §5.4, §11.2 layer 9).
//!
//! [`generate`] reads a Manim checkout **statically** (never importing or
//! executing it), walks `manim/**/*.py`, and extracts what a parser can
//! safely know: public classes and their base chains, method definitions,
//! returns-`self` evidence, module `__all__` lists, and the star-export
//! closure of the package `__init__`. The result is a [`GeneratedCandidates`]
//! document meant for human review — every entry is marked
//! `"generated": true` and curated-only semantic fields (effects,
//! introducer / remover flags, renderer notes) are **absent**: the generator
//! never invents semantics.
//!
//! [`diff`] compares candidates against a curated [`KnowledgeProfile`] and
//! reports three categories:
//!
//! - **(a) missing**: curated symbols not found in the source (possible
//!   upstream drift or a typo in the profile);
//! - **(b) contradictions**: curated `bases` / `returns_self` / `exports`
//!   facts the source positively contradicts — the DESIGN §11.2 layer-9
//!   drift gate fails on these;
//! - **(c) coverage gaps**: public star-exported API present in the source
//!   but absent from the profile, counted per defining module.
//!
//! Anything the source cannot positively confirm or deny stays a warning,
//! never a contradiction (DESIGN §15: prefer Unknown over a guess). Output
//! is byte-stable for identical input: sorted map keys, no timestamps.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use rustpython_parser::{Mode, ast, parse};
use serde::{Deserialize, Serialize};

use crate::frontend::imports::{ImportTarget, ImportedNames, import_bindings, import_from_names};
use crate::frontend::names::flatten_dotted;
use crate::frontend::parser::{ModuleIdentity, module_identity};
use crate::knowledge::model::{KnowledgeProfile, SymbolKind};

/// Schema version of the candidates document.
pub const CANDIDATES_SCHEMA_VERSION: u32 = 1;

/// Errors reading a Manim checkout from disk.
#[derive(Debug, thiserror::Error)]
pub enum GeneratorError {
    /// The given root does not contain a Python package.
    #[error(
        "no Python package found under `{root}`: expected `{root}/manim/__init__.py` \
         or `{root}/__init__.py`"
    )]
    PackageNotFound {
        /// The `--manim-root` path as given.
        root: String,
    },
    /// A file or directory could not be read.
    #[error("failed to read `{path}`: {message}")]
    Io {
        /// Path being read.
        path: String,
        /// Stringified I/O error.
        message: String,
    },
}

// ---------------------------------------------------------------------------
// Candidates document (serialized shape)
// ---------------------------------------------------------------------------

/// What kind of definition produced a candidate symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CandidateOrigin {
    /// A `class` statement.
    Class,
    /// A module-level `def`.
    Function,
    /// A module-level assignment.
    Constant,
}

/// Structural evidence about a method's `return` statements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReturnEvidence {
    /// At least one `return`, and every one is exactly `return self`.
    AllSelf,
    /// At least one `return`, and every one is provably not `self`
    /// (a bare `return` or a constant literal).
    NeverSelf,
    /// Returns exist but the evidence is mixed or opaque (calls,
    /// attributes, aliased names); no fact is claimed.
    Mixed,
    /// The body contains no `return` statement.
    NoReturns,
}

/// One extracted parameter (mirrors the curated `ParamFact` shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateParam {
    /// Parameter name as written.
    pub name: String,
    /// Whether the parameter has no default value.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
}

/// Extracted signature facts (mirrors the curated `SignatureFacts` shape).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateSignature {
    /// Named parameters in declaration order (`self` / `cls` dropped).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<CandidateParam>,
    /// The callable declares `*args`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub accepts_var_args: bool,
    /// The callable declares `**kwargs` (extra names may pass through).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub accepts_kwargs: bool,
}

/// One extracted method of a candidate class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateMethod {
    /// Always `true`: this entry was machine-extracted, not curated.
    pub generated: bool,
    /// Mirror of the curated `returns_self` field, present only when the
    /// evidence is decisive ([`ReturnEvidence::AllSelf`] → `true`,
    /// [`ReturnEvidence::NeverSelf`] → `false`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub returns_self: Option<bool>,
    /// Structural `return` evidence backing (or withholding) the fact.
    pub return_evidence: ReturnEvidence,
    /// Extracted signature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<CandidateSignature>,
    /// The method is decorated with `@property`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_property: bool,
}

/// One candidate symbol, keyed by canonical qualified ID
/// (same conventions as the curated profile).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateSymbol {
    /// Always `true`: this entry was machine-extracted, not curated.
    pub generated: bool,
    /// What kind of definition produced the symbol.
    pub origin: CandidateOrigin,
    /// Canonical IDs of the direct base classes that resolved statically,
    /// in source order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bases: Vec<String>,
    /// Base expressions that could not be resolved to a canonical ID,
    /// as written (dotted text or `<dynamic>`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved_bases: Vec<String>,
    /// For classes: the own `__init__` signature; for functions: the
    /// declared signature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<CandidateSignature>,
    /// Public methods defined directly on the class (`__init__` is
    /// promoted to `signature` instead of listed here).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub methods: BTreeMap<String, CandidateMethod>,
}

/// The machine-extracted candidates document (DESIGN §5.4).
///
/// Serialization is byte-stable for identical input: all maps are sorted,
/// there are no timestamps, and field order is fixed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedCandidates {
    /// Schema version of this document ([`CANDIDATES_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// Always `true`: the whole document is machine-extracted.
    pub generated: bool,
    /// Package name the candidates were read from (normally `manim`).
    pub package: String,
    /// `sha256:` digest of the per-file `sha256sum` manifest of every
    /// `*.py` under the package, byte-sorted by path — the recipe
    /// documented in `profiles/README.md`.
    pub source_digest: String,
    /// Star-export closure of the package `__init__`: public name →
    /// canonical defining symbol ID.
    pub exports: BTreeMap<String, String>,
    /// Public names in the closure that are module objects, mapped to the
    /// dotted module they refer to (internal or external).
    pub export_modules: BTreeMap<String, String>,
    /// Public names in the closure whose defining symbol could not be
    /// established statically (external symbols, dynamic re-exports).
    pub unresolved_exports: BTreeSet<String>,
    /// Whether the closure is complete: `false` when any star edge or
    /// `__all__` on the closure path was dynamic or unresolvable.
    pub exports_complete: bool,
    /// Statically extracted `__all__` lists, in source order, for modules
    /// that declare one.
    pub module_all: BTreeMap<String, Vec<String>>,
    /// Modules whose `__all__` is (also) manipulated dynamically; their
    /// static list above, if any, is a lower bound.
    pub dynamic_all_modules: BTreeSet<String>,
    /// Public top-level *defined* names (classes, functions, assignments —
    /// not imports) per module.
    pub module_members: BTreeMap<String, BTreeSet<String>>,
    /// Modules that failed to parse and contributed nothing.
    pub unparsed_modules: BTreeSet<String>,
    /// Candidate symbols keyed by canonical qualified ID. Classes are
    /// included regardless of name privacy (base chains need them);
    /// functions and constants are included when public.
    pub symbols: BTreeMap<String, CandidateSymbol>,
}

// ---------------------------------------------------------------------------
// Extraction: per-module raw facts
// ---------------------------------------------------------------------------

/// How a base class was written in source.
#[derive(Debug, Clone)]
enum BaseWritten {
    /// A plain name, possibly dotted (`Base`, `mob.Mobject`).
    Dotted(String, Vec<String>),
    /// Anything dynamic (a call, unpacking, subscript root, ...).
    Opaque,
}

/// One module-level name binding, in source order.
#[derive(Debug, Clone)]
enum RawBinding {
    Class,
    Function,
    Assign,
    Symbol { module: String, name: String },
    ModuleAlias(String),
    Unknown,
}

/// One namespace-affecting event of a module body.
#[derive(Debug, Clone)]
enum Event {
    Bind(String, RawBinding),
    Star(Option<String>),
}

#[derive(Debug, Clone)]
struct RawMethod {
    evidence: ReturnEvidence,
    signature: CandidateSignature,
    is_property: bool,
}

#[derive(Debug, Clone)]
struct RawClass {
    bases: Vec<BaseWritten>,
    /// Direct defs keyed by name; includes `__init__`, excludes other
    /// private names.
    methods: BTreeMap<String, RawMethod>,
}

#[derive(Debug, Default)]
struct ModuleFacts {
    events: Vec<Event>,
    all_names: Option<Vec<String>>,
    all_dynamic: bool,
    classes: BTreeMap<String, RawClass>,
    functions: BTreeMap<String, CandidateSignature>,
}

/// Extracts raw facts from one parsed module body.
fn extract_module(identity: &ModuleIdentity, body: &[ast::Stmt]) -> ModuleFacts {
    let mut facts = ModuleFacts::default();
    collect_events(body, identity, &mut facts);
    facts
}

fn collect_events(stmts: &[ast::Stmt], identity: &ModuleIdentity, facts: &mut ModuleFacts) {
    for stmt in stmts {
        match stmt {
            ast::Stmt::Import(import) => {
                for binding in import_bindings(import) {
                    let raw = match binding.target {
                        ImportTarget::Module(dotted) => RawBinding::ModuleAlias(dotted),
                        ImportTarget::Symbol { module, name } => {
                            RawBinding::Symbol { module, name }
                        }
                        ImportTarget::Unknown => RawBinding::Unknown,
                    };
                    facts.events.push(Event::Bind(binding.name, raw));
                }
            }
            ast::Stmt::ImportFrom(import) => match import_from_names(import, identity) {
                ImportedNames::Bindings(bindings) => {
                    for binding in bindings {
                        let raw = match binding.target {
                            ImportTarget::Module(dotted) => RawBinding::ModuleAlias(dotted),
                            ImportTarget::Symbol { module, name } => {
                                RawBinding::Symbol { module, name }
                            }
                            ImportTarget::Unknown => RawBinding::Unknown,
                        };
                        facts.events.push(Event::Bind(binding.name, raw));
                    }
                }
                ImportedNames::Star { module, .. } => facts.events.push(Event::Star(module)),
            },
            ast::Stmt::ClassDef(class) => {
                facts
                    .classes
                    .insert(class.name.to_string(), extract_class(class));
                facts
                    .events
                    .push(Event::Bind(class.name.to_string(), RawBinding::Class));
            }
            ast::Stmt::FunctionDef(def) => {
                record_function(facts, def.name.as_str(), &def.args, &def.decorator_list);
            }
            ast::Stmt::AsyncFunctionDef(def) => {
                record_function(facts, def.name.as_str(), &def.args, &def.decorator_list);
            }
            ast::Stmt::Assign(assign) => collect_assign(assign, facts),
            ast::Stmt::AnnAssign(assign) => {
                if assign.value.is_some() {
                    if let ast::Expr::Name(name) = assign.target.as_ref() {
                        facts
                            .events
                            .push(Event::Bind(name.id.to_string(), RawBinding::Assign));
                    }
                }
            }
            ast::Stmt::AugAssign(assign) => collect_aug_assign(assign, facts),
            ast::Stmt::Expr(expr) => {
                if is_dunder_all_mutation(&expr.value) {
                    facts.all_dynamic = true;
                }
            }
            ast::Stmt::Try(block) => {
                collect_events(&block.body, identity, facts);
                for ast::ExceptHandler::ExceptHandler(handler) in &block.handlers {
                    collect_events(&handler.body, identity, facts);
                }
                collect_events(&block.orelse, identity, facts);
                collect_events(&block.finalbody, identity, facts);
            }
            ast::Stmt::If(block) => {
                if !is_type_checking_guard(&block.test) {
                    collect_events(&block.body, identity, facts);
                    collect_events(&block.orelse, identity, facts);
                }
            }
            ast::Stmt::With(block) => collect_events(&block.body, identity, facts),
            _ => {}
        }
    }
}

fn record_function(
    facts: &mut ModuleFacts,
    name: &str,
    args: &ast::Arguments,
    decorators: &[ast::Expr],
) {
    if is_public(name) && !decorator_names(decorators).contains("overload") {
        facts
            .functions
            .insert(name.to_owned(), extract_signature(args, false));
    }
    facts
        .events
        .push(Event::Bind(name.to_owned(), RawBinding::Function));
}

fn collect_assign(assign: &ast::StmtAssign, facts: &mut ModuleFacts) {
    for target in &assign.targets {
        collect_assign_target(target, Some(&assign.value), facts);
    }
}

fn collect_assign_target(target: &ast::Expr, value: Option<&ast::Expr>, facts: &mut ModuleFacts) {
    match target {
        ast::Expr::Name(name) => {
            if name.id.as_str() == "__all__" {
                match value.and_then(static_string_list) {
                    Some(list) => match &mut facts.all_names {
                        Some(existing) => existing.extend(list),
                        None => facts.all_names = Some(list),
                    },
                    None => facts.all_dynamic = true,
                }
            } else {
                facts
                    .events
                    .push(Event::Bind(name.id.to_string(), RawBinding::Assign));
            }
        }
        ast::Expr::Tuple(tuple) => {
            for element in &tuple.elts {
                collect_assign_target(element, None, facts);
            }
        }
        ast::Expr::List(list) => {
            for element in &list.elts {
                collect_assign_target(element, None, facts);
            }
        }
        ast::Expr::Starred(starred) => collect_assign_target(&starred.value, None, facts),
        _ => {}
    }
}

fn collect_aug_assign(assign: &ast::StmtAugAssign, facts: &mut ModuleFacts) {
    if let ast::Expr::Name(name) = assign.target.as_ref() {
        if name.id.as_str() == "__all__" {
            match static_string_list(&assign.value) {
                Some(list) => match &mut facts.all_names {
                    Some(existing) => existing.extend(list),
                    None => facts.all_dynamic = true,
                },
                None => facts.all_dynamic = true,
            }
        } else {
            facts
                .events
                .push(Event::Bind(name.id.to_string(), RawBinding::Assign));
        }
    }
}

/// `__all__.append(...)` / `__all__.extend(...)` style mutation.
fn is_dunder_all_mutation(expr: &ast::Expr) -> bool {
    let ast::Expr::Call(call) = expr else {
        return false;
    };
    let ast::Expr::Attribute(attribute) = call.func.as_ref() else {
        return false;
    };
    matches!(attribute.value.as_ref(), ast::Expr::Name(name) if name.id.as_str() == "__all__")
}

/// `if TYPE_CHECKING:` (or `typing.TYPE_CHECKING`) — bindings under it do
/// not exist at runtime and are skipped.
fn is_type_checking_guard(test: &ast::Expr) -> bool {
    match test {
        ast::Expr::Name(name) => name.id.as_str() == "TYPE_CHECKING",
        ast::Expr::Attribute(attribute) => attribute.attr.as_str() == "TYPE_CHECKING",
        _ => false,
    }
}

/// A statically evaluable list/tuple of string literals.
fn static_string_list(expr: &ast::Expr) -> Option<Vec<String>> {
    let elements = match expr {
        ast::Expr::List(list) => &list.elts,
        ast::Expr::Tuple(tuple) => &tuple.elts,
        _ => return None,
    };
    elements
        .iter()
        .map(|element| match element {
            ast::Expr::Constant(constant) => match &constant.value {
                ast::Constant::Str(value) => Some(value.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn extract_class(class: &ast::StmtClassDef) -> RawClass {
    let bases = class.bases.iter().map(base_written).collect();
    let mut methods = BTreeMap::new();
    for stmt in &class.body {
        let (name, args, body, decorators) = match stmt {
            ast::Stmt::FunctionDef(def) => (&def.name, &def.args, &def.body, &def.decorator_list),
            ast::Stmt::AsyncFunctionDef(def) => {
                (&def.name, &def.args, &def.body, &def.decorator_list)
            }
            _ => continue,
        };
        if !is_public(name.as_str()) && name.as_str() != "__init__" {
            continue;
        }
        let decorator_set = decorator_names(decorators);
        if decorator_set.contains("overload") {
            continue;
        }
        let drop_first = !decorator_set.contains("staticmethod");
        methods.insert(
            name.to_string(),
            RawMethod {
                evidence: return_evidence(body),
                signature: extract_signature(args, drop_first),
                is_property: decorator_set.contains("property")
                    || decorator_set.contains("cached_property"),
            },
        );
    }
    RawClass { bases, methods }
}

fn base_written(expr: &ast::Expr) -> BaseWritten {
    // `Generic[T]`-style bases: look through the subscript.
    let expr = match expr {
        ast::Expr::Subscript(subscript) => subscript.value.as_ref(),
        other => other,
    };
    match flatten_dotted(expr) {
        Some((root, segments)) => BaseWritten::Dotted(root, segments),
        None => BaseWritten::Opaque,
    }
}

/// Last dotted segment of each decorator (looking through calls), e.g.
/// `@typing.overload` → `overload`, `@deprecated(...)` → `deprecated`.
fn decorator_names(decorators: &[ast::Expr]) -> BTreeSet<String> {
    decorators
        .iter()
        .filter_map(|decorator| {
            let target = match decorator {
                ast::Expr::Call(call) => call.func.as_ref(),
                other => other,
            };
            let (root, segments) = flatten_dotted(target)?;
            Some(segments.into_iter().next_back().unwrap_or(root))
        })
        .collect()
}

fn extract_signature(args: &ast::Arguments, drop_first: bool) -> CandidateSignature {
    let mut params: Vec<CandidateParam> = args
        .posonlyargs
        .iter()
        .chain(&args.args)
        .map(|arg| CandidateParam {
            name: arg.def.arg.to_string(),
            required: arg.default.is_none(),
        })
        .collect();
    if drop_first && !params.is_empty() {
        params.remove(0);
    }
    params.extend(args.kwonlyargs.iter().map(|arg| CandidateParam {
        name: arg.def.arg.to_string(),
        required: arg.default.is_none(),
    }));
    CandidateSignature {
        params,
        accepts_var_args: args.vararg.is_some(),
        accepts_kwargs: args.kwarg.is_some(),
    }
}

/// Classification of one `return` statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReturnKind {
    /// `return self`.
    SelfName,
    /// Bare `return` or a constant literal: provably not `self`.
    DefinitelyNot,
    /// A bare name other than `self` (could alias `self`) or any other
    /// expression (calls may return `self` transitively).
    Opaque,
}

fn return_evidence(body: &[ast::Stmt]) -> ReturnEvidence {
    let mut kinds = Vec::new();
    collect_returns(body, &mut kinds);
    if kinds.is_empty() {
        return ReturnEvidence::NoReturns;
    }
    if kinds.iter().all(|kind| *kind == ReturnKind::SelfName) {
        return ReturnEvidence::AllSelf;
    }
    if kinds.iter().all(|kind| *kind == ReturnKind::DefinitelyNot) {
        return ReturnEvidence::NeverSelf;
    }
    ReturnEvidence::Mixed
}

fn collect_returns(stmts: &[ast::Stmt], out: &mut Vec<ReturnKind>) {
    for stmt in stmts {
        match stmt {
            ast::Stmt::Return(ret) => out.push(classify_return(ret.value.as_deref())),
            ast::Stmt::If(block) => {
                collect_returns(&block.body, out);
                collect_returns(&block.orelse, out);
            }
            ast::Stmt::For(block) => {
                collect_returns(&block.body, out);
                collect_returns(&block.orelse, out);
            }
            ast::Stmt::AsyncFor(block) => {
                collect_returns(&block.body, out);
                collect_returns(&block.orelse, out);
            }
            ast::Stmt::While(block) => {
                collect_returns(&block.body, out);
                collect_returns(&block.orelse, out);
            }
            ast::Stmt::With(block) => collect_returns(&block.body, out),
            ast::Stmt::AsyncWith(block) => collect_returns(&block.body, out),
            ast::Stmt::Try(block) => {
                collect_returns(&block.body, out);
                for ast::ExceptHandler::ExceptHandler(handler) in &block.handlers {
                    collect_returns(&handler.body, out);
                }
                collect_returns(&block.orelse, out);
                collect_returns(&block.finalbody, out);
            }
            ast::Stmt::Match(block) => {
                for case in &block.cases {
                    collect_returns(&case.body, out);
                }
            }
            // Everything else, including nested function / class
            // definitions whose `return` statements have their own scope.
            _ => {}
        }
    }
}

fn classify_return(value: Option<&ast::Expr>) -> ReturnKind {
    match value {
        // Bare `return` and constant literals are provably not `self`.
        None | Some(ast::Expr::Constant(_)) => ReturnKind::DefinitelyNot,
        Some(ast::Expr::Name(name)) if name.id.as_str() == "self" => ReturnKind::SelfName,
        Some(_) => ReturnKind::Opaque,
    }
}

fn is_public(name: &str) -> bool {
    !name.starts_with('_')
}

// ---------------------------------------------------------------------------
// Namespace resolution (star-export closure)
// ---------------------------------------------------------------------------

/// Where one namespace name comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum NameOrigin {
    /// Defined inside the analyzed package; `module.name` is canonical.
    Defined {
        module: String,
        name: String,
        origin: CandidateOrigin,
    },
    /// A module object (internal or external).
    ModuleObject(String),
    /// A symbol imported from outside the package.
    External,
    /// Could not be established statically.
    Unknown,
}

#[derive(Debug, Clone, Default)]
struct Namespace {
    names: BTreeMap<String, NameOrigin>,
    complete: bool,
}

struct Resolver<'a> {
    modules: &'a BTreeMap<String, ModuleFacts>,
    package: &'a str,
    memo: BTreeMap<String, Namespace>,
    in_progress: BTreeSet<String>,
}

impl<'a> Resolver<'a> {
    fn new(modules: &'a BTreeMap<String, ModuleFacts>, package: &'a str) -> Self {
        Self {
            modules,
            package,
            memo: BTreeMap::new(),
            in_progress: BTreeSet::new(),
        }
    }

    fn is_internal(&self, module: &str) -> bool {
        module == self.package || module.starts_with(&format!("{}.", self.package))
    }

    fn namespace(&mut self, module: &str) -> Namespace {
        if let Some(cached) = self.memo.get(module) {
            return cached.clone();
        }
        if self.in_progress.contains(module) {
            // Import cycle: report an incomplete empty namespace to the
            // requester without poisoning the memo.
            return Namespace {
                names: BTreeMap::new(),
                complete: false,
            };
        }
        let Some(facts) = self.modules.get(module) else {
            return Namespace {
                names: BTreeMap::new(),
                complete: false,
            };
        };
        self.in_progress.insert(module.to_owned());
        let mut result = Namespace {
            names: BTreeMap::new(),
            complete: true,
        };
        // Split borrows: events are read while `self` recursses, so clone
        // the (small) event list up front.
        let events = facts.events.clone();
        for event in events {
            self.apply_event(module, event, &mut result);
        }
        self.in_progress.remove(module);
        self.memo.insert(module.to_owned(), result.clone());
        result
    }

    fn apply_event(&mut self, module: &str, event: Event, result: &mut Namespace) {
        match event {
            Event::Bind(name, raw) => {
                let origin = self.binding_origin(module, &name, raw);
                result.names.insert(name, origin);
            }
            Event::Star(Some(source)) if self.is_internal(&source) => {
                let (names, complete) = self.star_surface(&source);
                result.names.extend(names);
                result.complete &= complete;
            }
            Event::Star(_) => result.complete = false,
        }
    }

    fn binding_origin(&mut self, module: &str, name: &str, raw: RawBinding) -> NameOrigin {
        match raw {
            RawBinding::Class => NameOrigin::Defined {
                module: module.to_owned(),
                name: name.to_owned(),
                origin: CandidateOrigin::Class,
            },
            RawBinding::Function => NameOrigin::Defined {
                module: module.to_owned(),
                name: name.to_owned(),
                origin: CandidateOrigin::Function,
            },
            RawBinding::Assign => NameOrigin::Defined {
                module: module.to_owned(),
                name: name.to_owned(),
                origin: CandidateOrigin::Constant,
            },
            RawBinding::ModuleAlias(dotted) => NameOrigin::ModuleObject(dotted),
            RawBinding::Symbol {
                module: source,
                name: symbol,
            } => self.symbol_origin(&source, &symbol),
            RawBinding::Unknown => NameOrigin::Unknown,
        }
    }

    /// Resolves `from <source> import <symbol>` to the defining origin.
    fn symbol_origin(&mut self, source: &str, symbol: &str) -> NameOrigin {
        if !self.is_internal(source) {
            return NameOrigin::External;
        }
        let namespace = self.namespace(source);
        if let Some(origin) = namespace.names.get(symbol) {
            return origin.clone();
        }
        // `from package import submodule` falls back to the submodule when
        // the package namespace does not bind the name.
        let submodule = format!("{source}.{symbol}");
        if self.modules.contains_key(&submodule) {
            return NameOrigin::ModuleObject(submodule);
        }
        NameOrigin::Unknown
    }

    /// The names `from <module> import *` would bind, plus completeness.
    fn star_surface(&mut self, module: &str) -> (BTreeMap<String, NameOrigin>, bool) {
        let namespace = self.namespace(module);
        let Some(facts) = self.modules.get(module) else {
            return (BTreeMap::new(), false);
        };
        let complete = namespace.complete && !facts.all_dynamic;
        let names = if let Some(list) = facts.all_names.clone() {
            list.into_iter()
                .map(|name| {
                    let origin = namespace
                        .names
                        .get(&name)
                        .cloned()
                        .unwrap_or(NameOrigin::Unknown);
                    (name, origin)
                })
                .collect()
        } else {
            namespace
                .names
                .iter()
                .filter(|(name, _)| is_public(name))
                .map(|(name, origin)| (name.clone(), origin.clone()))
                .collect()
        };
        (names, complete)
    }
}

// ---------------------------------------------------------------------------
// Generation
// ---------------------------------------------------------------------------

/// Generates candidates by statically reading a Manim checkout.
///
/// `manim_root` is either the checkout root (containing `manim/__init__.py`)
/// or the package directory itself. Files are read once; nothing is ever
/// imported or executed. Modules that fail to parse are listed in
/// [`GeneratedCandidates::unparsed_modules`] and contribute nothing.
pub fn generate(manim_root: &Path) -> Result<GeneratedCandidates, GeneratorError> {
    let (package_dir, package) = locate_package(manim_root)?;
    let mut relative_paths = Vec::new();
    walk_py_files(&package_dir, Path::new(""), &mut relative_paths)?;
    relative_paths.sort();
    let mut files = Vec::with_capacity(relative_paths.len());
    for relative in relative_paths {
        let path = package_dir.join(&relative);
        let bytes = std::fs::read(&path).map_err(|error| GeneratorError::Io {
            path: path.display().to_string(),
            message: error.to_string(),
        })?;
        let posix = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        files.push((format!("{package}/{posix}"), bytes));
    }
    Ok(build_candidates(&package, &files))
}

/// Generates candidates from in-memory sources (for tests).
///
/// Each entry is `(relative POSIX path including the package directory,
/// source text)`, e.g. `("manim/mobject/mobject.py", "...")`.
#[must_use]
pub fn generate_from_sources(package: &str, files: &[(&str, &str)]) -> GeneratedCandidates {
    let owned: Vec<(String, Vec<u8>)> = files
        .iter()
        .map(|(path, source)| ((*path).to_owned(), source.as_bytes().to_vec()))
        .collect();
    build_candidates(package, &owned)
}

fn locate_package(root: &Path) -> Result<(PathBuf, String), GeneratorError> {
    let nested = root.join("manim");
    if nested.join("__init__.py").is_file() {
        return Ok((nested, "manim".to_owned()));
    }
    if root.join("__init__.py").is_file() {
        if let Some(name) = root.file_name().and_then(|name| name.to_str()) {
            return Ok((root.to_path_buf(), name.to_owned()));
        }
    }
    Err(GeneratorError::PackageNotFound {
        root: root.display().to_string(),
    })
}

fn walk_py_files(
    base: &Path,
    relative: &Path,
    out: &mut Vec<PathBuf>,
) -> Result<(), GeneratorError> {
    let dir = base.join(relative);
    let entries = std::fs::read_dir(&dir).map_err(|error| GeneratorError::Io {
        path: dir.display().to_string(),
        message: error.to_string(),
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| GeneratorError::Io {
            path: dir.display().to_string(),
            message: error.to_string(),
        })?;
        let name = entry.file_name();
        let child = relative.join(&name);
        let path = entry.path();
        if path.is_dir() {
            if name != "__pycache__" {
                walk_py_files(base, &child, out)?;
            }
        } else if path.extension().is_some_and(|extension| extension == "py") {
            out.push(child);
        }
    }
    Ok(())
}

/// Builds the candidates document from `(path, bytes)` pairs. Paths must be
/// POSIX-style and include the package directory (`manim/...`).
fn build_candidates(package: &str, files: &[(String, Vec<u8>)]) -> GeneratedCandidates {
    let mut sorted: Vec<&(String, Vec<u8>)> = files.iter().collect();
    sorted.sort_by(|left, right| left.0.cmp(&right.0));

    let mut manifest = String::new();
    let mut modules: BTreeMap<String, ModuleFacts> = BTreeMap::new();
    let mut unparsed_modules = BTreeSet::new();
    for (path, bytes) in sorted {
        let _ = writeln!(manifest, "{}  {path}", sha256_hex(bytes));
        let identity = module_identity(path, std::slice::from_ref(&".".to_owned()));
        let Ok(source) = std::str::from_utf8(bytes) else {
            unparsed_modules.insert(identity.name);
            continue;
        };
        match parse(source, Mode::Module, path) {
            Ok(ast::Mod::Module(module)) => {
                let facts = extract_module(&identity, &module.body);
                modules.insert(identity.name, facts);
            }
            _ => {
                unparsed_modules.insert(identity.name);
            }
        }
    }
    let source_digest = format!("sha256:{}", sha256_hex(manifest.as_bytes()));

    let mut resolver = Resolver::new(&modules, package);
    // The star surface (not the raw namespace) is what `from manim import *`
    // yields: it honors the package's own `__all__` and dynamic markers.
    let (closure_names, closure_complete) = resolver.star_surface(package);
    let symbols = build_symbols(&modules, &mut resolver, &closure_names);
    let (exports, export_modules, unresolved_exports) = split_exports(&closure_names);

    GeneratedCandidates {
        schema_version: CANDIDATES_SCHEMA_VERSION,
        generated: true,
        package: package.to_owned(),
        source_digest,
        exports,
        export_modules,
        unresolved_exports,
        exports_complete: closure_complete,
        module_all: modules
            .iter()
            .filter_map(|(name, facts)| facts.all_names.clone().map(|list| (name.clone(), list)))
            .collect(),
        dynamic_all_modules: modules
            .iter()
            .filter(|(_, facts)| facts.all_dynamic)
            .map(|(name, _)| name.clone())
            .collect(),
        module_members: modules
            .iter()
            .map(|(name, facts)| (name.clone(), public_members(facts)))
            .filter(|(_, members)| !members.is_empty())
            .collect(),
        unparsed_modules,
        symbols,
    }
}

fn public_members(facts: &ModuleFacts) -> BTreeSet<String> {
    facts
        .events
        .iter()
        .filter_map(|event| match event {
            Event::Bind(name, RawBinding::Class | RawBinding::Function | RawBinding::Assign)
                if is_public(name) =>
            {
                Some(name.clone())
            }
            _ => None,
        })
        .collect()
}

fn split_exports(
    closure: &BTreeMap<String, NameOrigin>,
) -> (
    BTreeMap<String, String>,
    BTreeMap<String, String>,
    BTreeSet<String>,
) {
    let mut exports = BTreeMap::new();
    let mut export_modules = BTreeMap::new();
    let mut unresolved = BTreeSet::new();
    for (name, origin) in closure {
        if !is_public(name) {
            continue;
        }
        match origin {
            NameOrigin::Defined {
                module,
                name: defined,
                ..
            } => {
                exports.insert(name.clone(), format!("{module}.{defined}"));
            }
            NameOrigin::ModuleObject(module) => {
                export_modules.insert(name.clone(), module.clone());
            }
            NameOrigin::External | NameOrigin::Unknown => {
                unresolved.insert(name.clone());
            }
        }
    }
    (exports, export_modules, unresolved)
}

fn build_symbols(
    modules: &BTreeMap<String, ModuleFacts>,
    resolver: &mut Resolver<'_>,
    closure: &BTreeMap<String, NameOrigin>,
) -> BTreeMap<String, CandidateSymbol> {
    let mut symbols = BTreeMap::new();
    for (module, facts) in modules {
        for (class_name, class) in &facts.classes {
            let id = format!("{module}.{class_name}");
            symbols.insert(id, class_symbol(module, class, resolver));
        }
        for (function_name, signature) in &facts.functions {
            let id = format!("{module}.{function_name}");
            symbols.insert(
                id,
                CandidateSymbol {
                    generated: true,
                    origin: CandidateOrigin::Function,
                    bases: Vec::new(),
                    unresolved_bases: Vec::new(),
                    signature: Some(signature.clone()),
                    methods: BTreeMap::new(),
                },
            );
        }
        // Constants declared public by a static `__all__`.
        if let Some(all_names) = &facts.all_names {
            let members = public_members(facts);
            for name in all_names {
                if members.contains(name) && constant_binding(facts, name) {
                    symbols
                        .entry(format!("{module}.{name}"))
                        .or_insert_with(constant_symbol);
                }
            }
        }
    }
    // Constants reachable through the star-export closure.
    for origin in closure.values() {
        if let NameOrigin::Defined {
            module,
            name,
            origin: CandidateOrigin::Constant,
        } = origin
        {
            symbols
                .entry(format!("{module}.{name}"))
                .or_insert_with(constant_symbol);
        }
    }
    symbols
}

fn constant_symbol() -> CandidateSymbol {
    CandidateSymbol {
        generated: true,
        origin: CandidateOrigin::Constant,
        bases: Vec::new(),
        unresolved_bases: Vec::new(),
        signature: None,
        methods: BTreeMap::new(),
    }
}

/// Whether the last binding of `name` in the module is a plain assignment.
fn constant_binding(facts: &ModuleFacts, name: &str) -> bool {
    facts
        .events
        .iter()
        .rev()
        .find_map(|event| match event {
            Event::Bind(bound, raw) if bound == name => Some(matches!(raw, RawBinding::Assign)),
            _ => None,
        })
        .unwrap_or(false)
}

fn class_symbol(module: &str, class: &RawClass, resolver: &mut Resolver<'_>) -> CandidateSymbol {
    let mut bases = Vec::new();
    let mut unresolved_bases = Vec::new();
    for base in &class.bases {
        match resolve_base(module, base, resolver) {
            Some(id) => bases.push(id),
            None => unresolved_bases.push(base_text(base)),
        }
    }
    let mut methods = BTreeMap::new();
    let mut signature = None;
    for (name, method) in &class.methods {
        if name == "__init__" {
            signature = Some(method.signature.clone());
            continue;
        }
        let returns_self = match method.evidence {
            ReturnEvidence::AllSelf => Some(true),
            ReturnEvidence::NeverSelf => Some(false),
            ReturnEvidence::Mixed | ReturnEvidence::NoReturns => None,
        };
        methods.insert(
            name.clone(),
            CandidateMethod {
                generated: true,
                returns_self,
                return_evidence: method.evidence,
                signature: Some(method.signature.clone()),
                is_property: method.is_property,
            },
        );
    }
    CandidateSymbol {
        generated: true,
        origin: CandidateOrigin::Class,
        bases,
        unresolved_bases,
        signature,
        methods,
    }
}

fn base_text(base: &BaseWritten) -> String {
    match base {
        BaseWritten::Dotted(root, segments) => {
            let mut text = root.clone();
            for segment in segments {
                text.push('.');
                text.push_str(segment);
            }
            text
        }
        BaseWritten::Opaque => "<dynamic>".to_owned(),
    }
}

/// Resolves one written base expression to a canonical class ID.
fn resolve_base(module: &str, base: &BaseWritten, resolver: &mut Resolver<'_>) -> Option<String> {
    let BaseWritten::Dotted(root, segments) = base else {
        return None;
    };
    let namespace = resolver.namespace(module);
    let root_origin = namespace.names.get(root)?.clone();
    if segments.is_empty() {
        return match root_origin {
            NameOrigin::Defined {
                module,
                name,
                origin: CandidateOrigin::Class,
            } => Some(format!("{module}.{name}")),
            _ => None,
        };
    }
    // Dotted base: walk module attributes (`alias.sub.Class`).
    let NameOrigin::ModuleObject(mut current) = root_origin else {
        return None;
    };
    if !resolver.is_internal(&current) {
        return None;
    }
    let (last, path) = segments.split_last()?;
    for segment in path {
        let submodule = format!("{current}.{segment}");
        if resolver.modules.contains_key(&submodule) {
            current = submodule;
            continue;
        }
        match resolver.namespace(&current).names.get(segment) {
            Some(NameOrigin::ModuleObject(next)) => current.clone_from(next),
            _ => return None,
        }
    }
    match resolver.symbol_origin(&current, last) {
        NameOrigin::Defined {
            module,
            name,
            origin: CandidateOrigin::Class,
        } => Some(format!("{module}.{name}")),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Drift report (diff mode)
// ---------------------------------------------------------------------------

/// A curated symbol not found in the source (category a).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingSymbol {
    /// Canonical qualified ID as curated.
    pub id: String,
    /// Curated kind (serialized lowercase name).
    pub kind: String,
    /// What was looked for and not found.
    pub detail: String,
}

/// A curated fact the source positively contradicts (category b).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contradiction {
    /// Canonical qualified ID (or export name for `exports` findings).
    pub id: String,
    /// Contradicted field: `bases`, `returns_self`, or `exports`.
    pub field: String,
    /// The curated claim.
    pub curated: String,
    /// What the source shows instead.
    pub observed: String,
}

/// Machine-readable drift report between candidates and a curated profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriftReport {
    /// Name of the curated profile compared against.
    pub profile: String,
    /// `source_digest` recorded in the curated profile.
    pub profile_digest: String,
    /// Digest of the source tree the candidates were generated from.
    pub source_digest: String,
    /// Whether the two digests match (informational, never a failure).
    pub digest_match: bool,
    /// Number of curated symbols checked.
    pub curated_symbols: usize,
    /// Number of curated exports checked.
    pub curated_exports: usize,
    /// Category (a): curated symbols missing from the source.
    pub missing: Vec<MissingSymbol>,
    /// Category (b): curated facts the source contradicts.
    pub contradictions: Vec<Contradiction>,
    /// Facts that could not be verified either way (kept as text).
    pub warnings: Vec<String>,
    /// Category (c): star-exported public names absent from the profile,
    /// grouped by defining module.
    pub coverage_gaps: BTreeMap<String, Vec<String>>,
    /// Total number of coverage-gap names.
    pub coverage_gap_total: usize,
}

impl DriftReport {
    /// Whether the layer-9 drift gate fails (any category-b contradiction).
    #[must_use]
    pub fn has_contradictions(&self) -> bool {
        !self.contradictions.is_empty()
    }

    /// Renders the deterministic human-readable report.
    #[must_use]
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "knowledge drift report");
        let _ = writeln!(out, "  profile: {}", self.profile);
        let _ = writeln!(out, "  profile digest: {}", self.profile_digest);
        let _ = writeln!(out, "  source digest:  {}", self.source_digest);
        let _ = writeln!(
            out,
            "  digest match: {} (informational)",
            if self.digest_match { "yes" } else { "no" }
        );
        let _ = writeln!(
            out,
            "  checked: {} curated symbols, {} curated exports",
            self.curated_symbols, self.curated_exports
        );
        let _ = writeln!(
            out,
            "(a) curated symbols missing from source: {}",
            self.missing.len()
        );
        for missing in &self.missing {
            let _ = writeln!(
                out,
                "  - {} ({}): {}",
                missing.id, missing.kind, missing.detail
            );
        }
        let _ = writeln!(out, "(b) contradictions: {}", self.contradictions.len());
        for contradiction in &self.contradictions {
            let _ = writeln!(
                out,
                "  - {} [{}]: curated {} ; observed {}",
                contradiction.id,
                contradiction.field,
                contradiction.curated,
                contradiction.observed
            );
        }
        let _ = writeln!(out, "unverifiable (warnings): {}", self.warnings.len());
        for warning in &self.warnings {
            let _ = writeln!(out, "  - {warning}");
        }
        let _ = writeln!(
            out,
            "(c) coverage gaps (star-exported, not in profile): {} across {} modules",
            self.coverage_gap_total,
            self.coverage_gaps.len()
        );
        for (module, names) in &self.coverage_gaps {
            let shown: Vec<&str> = names.iter().take(8).map(String::as_str).collect();
            let ellipsis = if names.len() > 8 { ", ..." } else { "" };
            let _ = writeln!(
                out,
                "  - {module}: {} ({}{ellipsis})",
                names.len(),
                shown.join(", ")
            );
        }
        out
    }
}

/// Result of resolving a curated method ID against the candidate classes.
enum MethodLookup<'a> {
    Found {
        defined_on: String,
        method: &'a CandidateMethod,
    },
    /// The full base chain resolved and the method is nowhere on it.
    NotFound,
    /// Some base could not be resolved; absence proves nothing.
    Unverifiable,
}

fn resolve_method<'a>(
    candidates: &'a GeneratedCandidates,
    class_id: &str,
    method: &str,
) -> MethodLookup<'a> {
    let mut stack = vec![class_id.to_owned()];
    let mut visited = BTreeSet::new();
    let mut unverifiable = false;
    while let Some(id) = stack.pop() {
        if !visited.insert(id.clone()) {
            continue;
        }
        let Some(symbol) = candidates.symbols.get(&id) else {
            unverifiable = true;
            continue;
        };
        if let Some(found) = symbol.methods.get(method) {
            return MethodLookup::Found {
                defined_on: id,
                method: found,
            };
        }
        if !symbol.unresolved_bases.is_empty() {
            unverifiable = true;
        }
        stack.extend(symbol.bases.iter().cloned());
    }
    if unverifiable {
        MethodLookup::Unverifiable
    } else {
        MethodLookup::NotFound
    }
}

/// All resolved transitive ancestors of a candidate class, plus whether the
/// chain resolved completely.
fn ancestor_closure(candidates: &GeneratedCandidates, class_id: &str) -> (BTreeSet<String>, bool) {
    let mut stack = vec![class_id.to_owned()];
    let mut visited = BTreeSet::new();
    let mut complete = true;
    while let Some(id) = stack.pop() {
        if !visited.insert(id.clone()) {
            continue;
        }
        let Some(symbol) = candidates.symbols.get(&id) else {
            complete = false;
            continue;
        };
        if !symbol.unresolved_bases.is_empty() {
            complete = false;
        }
        stack.extend(symbol.bases.iter().cloned());
    }
    visited.remove(class_id);
    (visited, complete)
}

fn split_qualified(id: &str) -> Option<(&str, &str)> {
    id.rsplit_once('.')
}

/// Compares candidates against a curated profile (DESIGN §11.2 layer 9).
#[must_use]
pub fn diff(candidates: &GeneratedCandidates, profile: &KnowledgeProfile) -> DriftReport {
    let mut report = DriftReport {
        profile: profile.name.clone(),
        profile_digest: profile.source_digest.clone(),
        source_digest: candidates.source_digest.clone(),
        digest_match: profile.source_digest == candidates.source_digest,
        curated_symbols: profile.symbols.len(),
        curated_exports: profile.exports.len(),
        missing: Vec::new(),
        contradictions: Vec::new(),
        warnings: Vec::new(),
        coverage_gaps: BTreeMap::new(),
        coverage_gap_total: 0,
    };
    for (id, entry) in &profile.symbols {
        match entry.kind {
            SymbolKind::Method => check_method(candidates, id, entry.returns_self, &mut report),
            SymbolKind::Constant | SymbolKind::Function => {
                check_member(candidates, id, entry.kind, &mut report);
            }
            SymbolKind::Scene
            | SymbolKind::Mobject
            | SymbolKind::Vmobject
            | SymbolKind::Animation
            | SymbolKind::Camera => {
                check_class(candidates, id, entry.kind, &entry.bases, &mut report);
            }
        }
    }
    check_exports(candidates, profile, &mut report);
    collect_coverage_gaps(candidates, profile, &mut report);
    report
}

fn kind_label(kind: SymbolKind) -> String {
    format!("{kind:?}").to_lowercase()
}

fn check_method(
    candidates: &GeneratedCandidates,
    id: &str,
    curated_returns_self: Option<bool>,
    report: &mut DriftReport,
) {
    let Some((class_id, method_name)) = split_qualified(id) else {
        report
            .warnings
            .push(format!("`{id}`: not a dotted method id"));
        return;
    };
    match resolve_method(candidates, class_id, method_name) {
        MethodLookup::Found { defined_on, method } => {
            match (curated_returns_self, method.return_evidence) {
                (Some(true), ReturnEvidence::NeverSelf) => {
                    report.contradictions.push(Contradiction {
                        id: id.to_owned(),
                        field: "returns_self".to_owned(),
                        curated: "true".to_owned(),
                        observed: format!(
                            "every return in `{defined_on}.{method_name}` is a bare return \
                             or constant, never `self`"
                        ),
                    });
                }
                (Some(false), ReturnEvidence::AllSelf) => {
                    report.contradictions.push(Contradiction {
                        id: id.to_owned(),
                        field: "returns_self".to_owned(),
                        curated: "false".to_owned(),
                        observed: format!(
                            "every return in `{defined_on}.{method_name}` is `return self`"
                        ),
                    });
                }
                _ => {}
            }
        }
        MethodLookup::NotFound => report.missing.push(MissingSymbol {
            id: id.to_owned(),
            kind: "method".to_owned(),
            detail: format!(
                "method `{method_name}` not found on `{class_id}` or any resolved ancestor"
            ),
        }),
        MethodLookup::Unverifiable => report.warnings.push(format!(
            "`{id}`: method not found but the base chain of `{class_id}` did not fully resolve"
        )),
    }
}

fn check_member(
    candidates: &GeneratedCandidates,
    id: &str,
    kind: SymbolKind,
    report: &mut DriftReport,
) {
    if candidates.symbols.contains_key(id) {
        return;
    }
    let Some((module, name)) = split_qualified(id) else {
        report
            .warnings
            .push(format!("`{id}`: not a dotted symbol id"));
        return;
    };
    if candidates
        .module_members
        .get(module)
        .is_some_and(|members| members.contains(name))
    {
        return;
    }
    if candidates.unparsed_modules.contains(module) {
        report
            .warnings
            .push(format!("`{id}`: module `{module}` failed to parse"));
        return;
    }
    report.missing.push(MissingSymbol {
        id: id.to_owned(),
        kind: kind_label(kind),
        detail: format!("no top-level definition `{name}` in module `{module}`"),
    });
}

fn check_class(
    candidates: &GeneratedCandidates,
    id: &str,
    kind: SymbolKind,
    curated_bases: &[String],
    report: &mut DriftReport,
) {
    let Some(symbol) = candidates.symbols.get(id) else {
        check_member(candidates, id, kind, report);
        return;
    };
    if curated_bases.is_empty() {
        return;
    }
    let direct: BTreeSet<&String> = symbol.bases.iter().collect();
    let (ancestors, complete) = ancestor_closure(candidates, id);
    for curated_base in curated_bases {
        if direct.contains(curated_base) || ancestors.contains(curated_base) {
            continue;
        }
        if complete && symbol.unresolved_bases.is_empty() {
            report.contradictions.push(Contradiction {
                id: id.to_owned(),
                field: "bases".to_owned(),
                curated: curated_base.clone(),
                observed: format!(
                    "resolved base chain is [{}]",
                    ancestors.iter().cloned().collect::<Vec<_>>().join(", ")
                ),
            });
        } else {
            report.warnings.push(format!(
                "`{id}`: curated base `{curated_base}` not confirmed; base chain did not \
                 fully resolve"
            ));
        }
    }
}

fn check_exports(
    candidates: &GeneratedCandidates,
    profile: &KnowledgeProfile,
    report: &mut DriftReport,
) {
    for (name, curated_id) in &profile.exports {
        if let Some(observed_id) = candidates.exports.get(name) {
            if observed_id != curated_id {
                report.contradictions.push(Contradiction {
                    id: name.clone(),
                    field: "exports".to_owned(),
                    curated: curated_id.clone(),
                    observed: observed_id.clone(),
                });
            }
            continue;
        }
        if candidates.unresolved_exports.contains(name)
            || candidates.export_modules.contains_key(name)
        {
            report.warnings.push(format!(
                "export `{name}`: present in the star surface but its defining symbol \
                 could not be established statically"
            ));
        } else if candidates.exports_complete {
            report.contradictions.push(Contradiction {
                id: name.clone(),
                field: "exports".to_owned(),
                curated: curated_id.clone(),
                observed: "name is not star-exported by the package `__init__`".to_owned(),
            });
        } else {
            report.warnings.push(format!(
                "export `{name}`: not found, but the star-export closure is incomplete"
            ));
        }
    }
}

fn collect_coverage_gaps(
    candidates: &GeneratedCandidates,
    profile: &KnowledgeProfile,
    report: &mut DriftReport,
) {
    for (name, id) in &candidates.exports {
        if profile.exports.contains_key(name) || profile.symbols.contains_key(id) {
            continue;
        }
        let module = split_qualified(id).map_or("<root>", |(module, _)| module);
        report
            .coverage_gaps
            .entry(module.to_owned())
            .or_default()
            .push(name.clone());
    }
    report.coverage_gap_total = report.coverage_gaps.values().map(Vec::len).sum();
}

// ---------------------------------------------------------------------------
// SHA-256 (no external dependency; used only for the source digest)
// ---------------------------------------------------------------------------

/// SHA-256 round constants (FIPS 180-4 §4.2.2).
const SHA256_K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// Hex SHA-256 digest of `bytes`.
///
/// A minimal, safe FIPS 180-4 implementation kept private to the generator
/// so the analyzer gains no new crate dependency for its source-digest
/// recipe (see `profiles/README.md`).
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut state: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    let bit_length = u64::try_from(bytes.len())
        .unwrap_or(u64::MAX)
        .wrapping_mul(8);
    let mut message = bytes.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_length.to_be_bytes());

    for chunk in message.chunks_exact(64) {
        sha256_compress(&mut state, chunk);
    }
    let mut hex = String::with_capacity(64);
    for word in state {
        let _ = write!(hex, "{word:08x}");
    }
    hex
}

/// One SHA-256 compression round over a 64-byte block.
fn sha256_compress(state: &mut [u32; 8], chunk: &[u8]) {
    let mut schedule = [0u32; 64];
    for (index, word) in chunk.chunks_exact(4).enumerate() {
        schedule[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
    }
    for index in 16..64 {
        let sigma0 = schedule[index - 15].rotate_right(7)
            ^ schedule[index - 15].rotate_right(18)
            ^ (schedule[index - 15] >> 3);
        let sigma1 = schedule[index - 2].rotate_right(17)
            ^ schedule[index - 2].rotate_right(19)
            ^ (schedule[index - 2] >> 10);
        schedule[index] = schedule[index - 16]
            .wrapping_add(sigma0)
            .wrapping_add(schedule[index - 7])
            .wrapping_add(sigma1);
    }
    // Working variables a..h of FIPS 180-4 §6.2.2, spelled va..vh.
    let [
        mut va,
        mut vb,
        mut vc,
        mut vd,
        mut ve,
        mut vf,
        mut vg,
        mut vh,
    ] = *state;
    for index in 0..64 {
        let big_sigma1 = ve.rotate_right(6) ^ ve.rotate_right(11) ^ ve.rotate_right(25);
        let choose = (ve & vf) ^ (!ve & vg);
        let temp1 = vh
            .wrapping_add(big_sigma1)
            .wrapping_add(choose)
            .wrapping_add(SHA256_K[index])
            .wrapping_add(schedule[index]);
        let big_sigma0 = va.rotate_right(2) ^ va.rotate_right(13) ^ va.rotate_right(22);
        let majority = (va & vb) ^ (va & vc) ^ (vb & vc);
        let temp2 = big_sigma0.wrapping_add(majority);
        vh = vg;
        vg = vf;
        vf = ve;
        ve = vd.wrapping_add(temp1);
        vd = vc;
        vc = vb;
        vb = va;
        va = temp1.wrapping_add(temp2);
    }
    let updates = [va, vb, vc, vd, ve, vf, vg, vh];
    for (slot, update) in state.iter_mut().zip(updates) {
        *slot = slot.wrapping_add(update);
    }
}

/// Serializes candidates or reports as pretty JSON with a trailing newline
/// (byte-stable: sorted map keys, fixed field order, no timestamps).
pub fn to_stable_json<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let mut json = serde_json::to_string_pretty(value)?;
    json.push('\n');
    Ok(json)
}
