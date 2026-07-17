//! Project-wide index and qualified call facts (DESIGN §5.1, §5.3).
//!
//! Builds, over all parsed files of a [`SourceManager`]:
//!
//! - the module tree (dotted names derived from the configured source roots),
//! - per-module namespaces and export surfaces (`__all__` respected when
//!   it is a literal),
//! - the class hierarchy with bases resolved to qualified names where
//!   possible, and Scene / Mobject / Animation subclass discovery driven by
//!   the [`ManimSurface`] base sets — never by the string `"Scene"` alone,
//! - qualified call facts: for every `Call` expression, the set of candidate
//!   fully qualified targets plus receiver classification, argument summary,
//!   and enclosing context.

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::ast::{self, Ranged};
use rustpython_parser::text_size::TextRange;

use super::ManimSurface;
use super::imports::{ImportTarget, ImportedNames, import_bindings, import_from_names};
use super::names::{Binding, LocalValue, ScopeStack, assigned_names, flatten_dotted};
use super::parser::{ModuleIdentity, ParsedModule, module_identity, parsed_modules};
use crate::source::{FileId, SourceFile, SourceManager};

/// Which Manim base kind a project class was discovered to reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ManimKind {
    /// Reaches a `ManimSurface::scene_bases` id.
    Scene,
    /// Reaches a `ManimSurface::mobject_bases` id.
    Mobject,
    /// Reaches a `ManimSurface::animation_bases` id.
    Animation,
}

/// One base-class reference of a project class.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum BaseRef {
    /// Resolved to a qualified id: either a project class
    /// (`my_scenes.base.BaseScene`) or a canonical external symbol
    /// (`manim.scene.scene.Scene`).
    Resolved(String),
    /// Could not be resolved; carries the base expression's source text.
    /// Never treated as any concrete class.
    Unresolved(String),
}

/// A class defined at module (or nested class) level in the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassRecord {
    /// Fully qualified id, e.g. `my_scenes.base.BaseScene`.
    pub qualified_name: String,
    /// Bare class name.
    pub name: String,
    /// File the class is defined in.
    pub file: FileId,
    /// Byte range of the whole `class` statement.
    pub range: TextRange,
    /// Base-class references in declaration order.
    pub bases: Vec<BaseRef>,
    /// Methods defined directly on this class, name → def byte range.
    pub methods: BTreeMap<String, TextRange>,
    /// Manim kinds reached through the resolved base chain.
    pub kinds: BTreeSet<ManimKind>,
    /// Canonical external base ids reached through the chain (project
    /// ancestors are expanded, external ids are kept).
    pub reached_bases: BTreeSet<String>,
    /// False when any base in the chain is [`BaseRef::Unresolved`]; the
    /// class may reach more bases than discovered.
    pub bases_fully_resolved: bool,
}

/// Export surface of one module.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportSurface {
    /// Names exported by `from module import *`.
    pub names: BTreeSet<String>,
    /// True when the names come from a literal `__all__`.
    pub from_literal_all: bool,
    /// False when unresolved star imports (or a dynamic `__all__`) may add
    /// names this index does not know about.
    pub complete: bool,
}

/// One module of the project tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleRecord {
    /// File backing the module.
    pub file: FileId,
    /// Dotted module name.
    pub name: String,
    /// Whether the module is a package `__init__.py`.
    pub is_package: bool,
    /// Final module-level namespace: name → binding.
    pub namespace: BTreeMap<String, Binding>,
    /// True when an unresolved star import may bind additional names.
    pub has_unknown_star: bool,
    /// The module's star-import export surface.
    pub exports: ExportSurface,
}

/// Project-wide module / symbol / class-hierarchy index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectIndex {
    /// Modules by dotted name.
    pub modules: BTreeMap<String, ModuleRecord>,
    /// Module identity of every parsed file (also files that lost a
    /// module-name collision and are absent from `modules`).
    pub module_of_file: BTreeMap<FileId, ModuleIdentity>,
    /// All dotted module names plus implied ancestor package names.
    pub module_tree: BTreeSet<String>,
    /// Classes by qualified name.
    pub classes: BTreeMap<String, ClassRecord>,
    /// Qualified names of discovered Manim Scene subclasses.
    pub scene_classes: BTreeSet<String>,
    /// Qualified names of discovered Manim Mobject subclasses.
    pub mobject_classes: BTreeSet<String>,
    /// Qualified names of discovered Manim Animation subclasses.
    pub animation_classes: BTreeSet<String>,
}

impl ProjectIndex {
    /// Builds the index over all parsed files of `sources`.
    ///
    /// `source_roots` are the project-relative import roots from the resolved
    /// configuration; `surface` is the Manim API surface the resolver uses.
    #[must_use]
    pub fn build(sources: &SourceManager, source_roots: &[String], surface: &ManimSurface) -> Self {
        let parsed = parsed_modules(sources);
        build_index(&parsed, source_roots, surface)
    }

    /// Whether `qualified_name` is a discovered Manim Scene subclass.
    #[must_use]
    pub fn is_scene_class(&self, qualified_name: &str) -> bool {
        self.scene_classes.contains(qualified_name)
    }

    /// Candidate qualified targets for calling `method` on an instance of
    /// `kind` (a project class id or a canonical external id).
    ///
    /// A project class chain is searched first (nearest override wins); when
    /// no project ancestor defines the method, one candidate per reached
    /// canonical base is returned. For an external id the candidate is
    /// `kind.method` — mapping it further onto the real MRO is knowledge
    /// profile territory, not the frontend's.
    #[must_use]
    pub fn method_candidates(&self, kind: &str, method: &str) -> BTreeSet<String> {
        if !self.classes.contains_key(kind) {
            return BTreeSet::from([format!("{kind}.{method}")]);
        }
        // Breadth-first over the project ancestor chain, declaration order.
        let mut queue = vec![kind.to_owned()];
        let mut visited = BTreeSet::new();
        let mut cursor = 0;
        while cursor < queue.len() {
            let current = queue[cursor].clone();
            cursor += 1;
            if !visited.insert(current.clone()) {
                continue;
            }
            let Some(class) = self.classes.get(&current) else {
                continue;
            };
            if class.methods.contains_key(method) {
                return BTreeSet::from([format!("{current}.{method}")]);
            }
            for base in &class.bases {
                if let BaseRef::Resolved(id) = base {
                    if self.classes.contains_key(id) {
                        queue.push(id.clone());
                    }
                }
            }
        }
        self.classes[kind]
            .reached_bases
            .iter()
            .map(|base| format!("{base}.{method}"))
            .collect()
    }
}

/// How the callee expression of a call is anchored.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReceiverKind {
    /// `self.method(...)` inside a method of a discovered Scene subclass.
    SelfScene,
    /// Attribute call on a value of a known class; carries the class id
    /// (project qualified or canonical external).
    KnownInstance(String),
    /// Attribute call through a module binding (`mn.FadeIn(...)`).
    ModuleAlias,
    /// A bare name (`FadeIn(...)`).
    Direct,
    /// The receiver could not be classified.
    Unknown,
}

/// Shape of one call argument.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ArgShape {
    /// The argument is itself a call; carries the index of its own
    /// [`QualifiedCall`] fact inside [`QualifiedCallFacts::calls`].
    Call(usize),
    /// A literal constant (including f-strings).
    Literal,
    /// A plain name.
    Name,
    /// A lambda expression.
    Lambda,
    /// Anything else (attribute chains, starred args, comprehensions, ...).
    Other,
}

/// One argument of a call, in source order (positional then keyword).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallArgument {
    /// Keyword name, `None` for positional and `**kwargs` arguments.
    pub keyword: Option<String>,
    /// Shape classification.
    pub shape: ArgShape,
    /// Byte range of the argument expression.
    pub range: TextRange,
}

/// Enclosing context of a call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallContext {
    /// Dotted module name.
    pub module: String,
    /// Qualified name of the innermost enclosing class, if any.
    pub class_name: Option<String>,
    /// Dotted function path inside the module/class (e.g. `construct` or
    /// `construct.helper`), if the call is inside a function or method.
    pub function: Option<String>,
}

/// One resolved call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualifiedCall {
    /// File containing the call.
    pub file: FileId,
    /// Byte range of the whole call expression.
    pub call_range: TextRange,
    /// Byte range of the callee expression.
    pub callee_range: TextRange,
    /// Candidate fully qualified targets. Empty when nothing could be
    /// resolved; resolution never guesses.
    pub candidates: BTreeSet<String>,
    /// Receiver classification.
    pub receiver: ReceiverKind,
    /// Number of positional arguments (including starred).
    pub positional_count: usize,
    /// Names of keyword arguments.
    pub keyword_names: BTreeSet<String>,
    /// Every argument in source order.
    pub arguments: Vec<CallArgument>,
    /// Enclosing module / class / method.
    pub context: CallContext,
}

/// All qualified call facts of the project, in file / source order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QualifiedCallFacts {
    /// Facts in deterministic walk order (files in load order; within a
    /// file, post-order per statement so nested calls precede their parent).
    pub calls: Vec<QualifiedCall>,
}

impl QualifiedCallFacts {
    /// Collects call facts for every parsed file using a built index.
    #[must_use]
    pub fn collect(sources: &SourceManager, index: &ProjectIndex, surface: &ManimSurface) -> Self {
        let parsed = parsed_modules(sources);
        collect_calls(&parsed, index, surface)
    }

    /// Facts inside one file, in source walk order.
    pub fn calls_in_file(&self, file: FileId) -> impl Iterator<Item = &QualifiedCall> {
        self.calls.iter().filter(move |call| call.file == file)
    }

    /// Facts whose candidate set contains `target`.
    pub fn calls_with_candidate<'a>(
        &'a self,
        target: &'a str,
    ) -> impl Iterator<Item = &'a QualifiedCall> {
        self.calls
            .iter()
            .filter(move |call| call.candidates.contains(target))
    }
}

/// Everything the frontend derives from the parsed project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrontendFacts {
    /// Module / symbol / class-hierarchy index.
    pub index: ProjectIndex,
    /// Qualified call facts.
    pub calls: QualifiedCallFacts,
}

/// Runs the whole frontend: index construction and call-fact collection.
#[must_use]
pub fn analyze(
    sources: &SourceManager,
    source_roots: &[String],
    surface: &ManimSurface,
) -> FrontendFacts {
    let index = ProjectIndex::build(sources, source_roots, surface);
    let calls = QualifiedCallFacts::collect(sources, &index, surface);
    FrontendFacts { index, calls }
}

// ---------------------------------------------------------------------------
// Namespace resolution shared by the fixpoint pass and the call walk.
// ---------------------------------------------------------------------------

/// Read access to the module namespaces available while resolving.
trait ModuleSpaces {
    /// Whether `module` is part of the project module tree (including
    /// implied ancestor packages).
    fn module_exists(&self, module: &str) -> bool;
    /// Binding of `name` inside project module `module`, if known.
    fn binding(&self, module: &str, name: &str) -> Option<Binding>;
    /// Star-export surface of project module `module`, if known.
    fn export_names(&self, module: &str) -> Option<BTreeSet<String>>;
    /// Whether `module`'s namespace may miss names (unresolved stars).
    fn star_incomplete(&self, module: &str) -> bool;
}

#[derive(Clone, PartialEq, Eq)]
struct ModuleState {
    namespace: BTreeMap<String, Binding>,
    has_unknown_star: bool,
    exports: ExportSurface,
}

struct SnapshotSpaces<'a> {
    states: &'a BTreeMap<String, ModuleState>,
    tree: &'a BTreeSet<String>,
}

impl ModuleSpaces for SnapshotSpaces<'_> {
    fn module_exists(&self, module: &str) -> bool {
        self.tree.contains(module)
    }

    fn binding(&self, module: &str, name: &str) -> Option<Binding> {
        self.states
            .get(module)
            .and_then(|state| state.namespace.get(name).cloned())
    }

    fn export_names(&self, module: &str) -> Option<BTreeSet<String>> {
        self.states
            .get(module)
            .map(|state| state.exports.names.clone())
    }

    fn star_incomplete(&self, module: &str) -> bool {
        self.states
            .get(module)
            .is_none_or(|state| !state.exports.complete)
    }
}

struct IndexSpaces<'a> {
    index: &'a ProjectIndex,
}

impl ModuleSpaces for IndexSpaces<'_> {
    fn module_exists(&self, module: &str) -> bool {
        self.index.module_tree.contains(module)
    }

    fn binding(&self, module: &str, name: &str) -> Option<Binding> {
        self.index
            .modules
            .get(module)
            .and_then(|record| record.namespace.get(name).cloned())
    }

    fn export_names(&self, module: &str) -> Option<BTreeSet<String>> {
        self.index
            .modules
            .get(module)
            .map(|record| record.exports.names.clone())
    }

    fn star_incomplete(&self, module: &str) -> bool {
        self.index
            .modules
            .get(module)
            .is_none_or(|record| !record.exports.complete)
    }
}

/// Resolves imported symbols against the Manim surface and project tree.
struct Resolver<'a> {
    surface: &'a ManimSurface,
}

impl Resolver<'_> {
    /// Binding of `from module import name` (also used for `alias.attr`
    /// chains, where `module` is the flattened parent path).
    fn symbol_import(&self, spaces: &dyn ModuleSpaces, module: &str, name: &str) -> Binding {
        if module == "manim" {
            return match self.surface.star_exports.get(name) {
                Some(canonical) => Binding::ImportedSymbol(canonical.clone()),
                // Could be a Manim submodule or private name; never guess.
                None => Binding::Unknown,
            };
        }
        if module.starts_with("manim.") {
            return Binding::ImportedSymbol(format!("{module}.{name}"));
        }
        let dotted = format!("{module}.{name}");
        if spaces.module_exists(&dotted) {
            return Binding::ImportedModule(dotted);
        }
        if spaces.module_exists(module) {
            return spaces.binding(module, name).unwrap_or(Binding::Unknown);
        }
        let top = module.split('.').next().unwrap_or(module);
        if spaces.module_exists(top) {
            // Inside the project tree but the module is missing: Unknown.
            return Binding::Unknown;
        }
        // External third-party module: record the dotted path it was
        // imported from as the qualified id.
        Binding::ImportedSymbol(dotted)
    }

    /// Binding of a dotted attribute path rooted at module `module`.
    fn module_attr(&self, spaces: &dyn ModuleSpaces, module: &str, segments: &[String]) -> Binding {
        let Some((last, parents)) = segments.split_last() else {
            return Binding::ImportedModule(module.to_owned());
        };
        let parent = if parents.is_empty() {
            module.to_owned()
        } else {
            format!("{module}.{}", parents.join("."))
        };
        self.symbol_import(spaces, &parent, last)
    }
}

// ---------------------------------------------------------------------------
// Module walker (shared by the namespace fixpoint and the call-fact pass).
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum WalkMode {
    /// Compute bindings, exports, and class records; skip function bodies.
    Bind,
    /// Emit call facts; walk function and method bodies.
    Full,
}

struct Walker<'a> {
    resolver: Resolver<'a>,
    spaces: &'a dyn ModuleSpaces,
    identity: &'a ModuleIdentity,
    source: &'a SourceFile,
    file: FileId,
    mode: WalkMode,
    /// Built index (Full mode only) for scene checks and method candidates.
    index: Option<&'a ProjectIndex>,
    /// Final module namespace, the base scope of function bodies.
    final_ns: &'a BTreeMap<String, Binding>,
    module_unknown_star: bool,
    facts: Vec<QualifiedCall>,
    class_records: Vec<ClassRecord>,
    all_names: Option<BTreeSet<String>>,
    all_invalid: bool,
    class_stack: Vec<String>,
    function_stack: Vec<String>,
    /// Class whose method body is currently being walked.
    method_class: Option<String>,
}

impl Walker<'_> {
    fn qualified(&self, name: &str) -> String {
        let mut parts = vec![self.identity.name.clone()];
        parts.extend(self.class_stack.iter().map(|class| {
            class
                .rsplit('.')
                .next()
                .unwrap_or(class.as_str())
                .to_owned()
        }));
        parts.extend(self.function_stack.iter().cloned());
        parts.push(name.to_owned());
        parts.join(".")
    }

    fn context(&self) -> CallContext {
        CallContext {
            module: self.identity.name.clone(),
            class_name: self.class_stack.last().cloned(),
            function: if self.function_stack.is_empty() {
                None
            } else {
                Some(self.function_stack.join("."))
            },
        }
    }

    // -- statements --------------------------------------------------------

    fn walk_stmts(&mut self, stmts: &[ast::Stmt], scopes: &mut ScopeStack<'_>) {
        for stmt in stmts {
            self.walk_stmt(stmt, scopes);
        }
    }

    #[allow(clippy::too_many_lines, reason = "one arm per statement kind")]
    fn walk_stmt(&mut self, stmt: &ast::Stmt, scopes: &mut ScopeStack<'_>) {
        match stmt {
            ast::Stmt::Import(import) => {
                for binding in import_bindings(import) {
                    let bound = match binding.target {
                        ImportTarget::Module(module) => Binding::ImportedModule(module),
                        ImportTarget::Symbol { module, name } => {
                            self.resolver.symbol_import(self.spaces, &module, &name)
                        }
                        ImportTarget::Unknown => Binding::Unknown,
                    };
                    scopes.bind(&binding.name, bound);
                }
            }
            ast::Stmt::ImportFrom(import) => match import_from_names(import, self.identity) {
                ImportedNames::Bindings(bindings) => {
                    for binding in bindings {
                        let bound = match binding.target {
                            ImportTarget::Module(module) => Binding::ImportedModule(module),
                            ImportTarget::Symbol { module, name } => {
                                self.resolver.symbol_import(self.spaces, &module, &name)
                            }
                            ImportTarget::Unknown => Binding::Unknown,
                        };
                        scopes.bind(&binding.name, bound);
                    }
                }
                ImportedNames::Star { module, .. } => {
                    self.expand_star(module.as_deref(), scopes);
                }
            },
            ast::Stmt::FunctionDef(def) => {
                self.walk_function_def(
                    &def.name,
                    &def.args,
                    &def.body,
                    &def.decorator_list,
                    scopes,
                );
            }
            ast::Stmt::AsyncFunctionDef(def) => {
                self.walk_function_def(
                    &def.name,
                    &def.args,
                    &def.body,
                    &def.decorator_list,
                    scopes,
                );
            }
            ast::Stmt::ClassDef(def) => self.walk_class_def(def, scopes),
            ast::Stmt::Assign(assign) => {
                self.scan(&assign.value, scopes);
                if self.mode == WalkMode::Bind {
                    if let [ast::Expr::Name(target)] = assign.targets.as_slice() {
                        if target.id.as_str() == "__all__" {
                            match literal_all(&assign.value) {
                                Some(names) => self.all_names = Some(names),
                                None => self.all_invalid = true,
                            }
                        }
                    }
                }
                let value = self.classify_value(&assign.value, scopes);
                for target in &assign.targets {
                    bind_target(target, &value, scopes);
                }
            }
            ast::Stmt::AnnAssign(assign) => {
                if let Some(value) = &assign.value {
                    self.scan(value, scopes);
                    let bound = self.classify_value(value, scopes);
                    bind_target(&assign.target, &bound, scopes);
                }
            }
            ast::Stmt::AugAssign(assign) => {
                self.scan(&assign.value, scopes);
                if let ast::Expr::Name(target) = assign.target.as_ref() {
                    if self.mode == WalkMode::Bind && target.id.as_str() == "__all__" {
                        self.all_invalid = true;
                    }
                    scopes.bind(target.id.as_str(), Binding::LocalVar(LocalValue::Opaque));
                }
            }
            ast::Stmt::Expr(expr) => {
                self.scan(&expr.value, scopes);
            }
            ast::Stmt::Return(ret) => {
                if let Some(value) = &ret.value {
                    self.scan(value, scopes);
                }
            }
            ast::Stmt::Raise(raise) => {
                if let Some(exc) = &raise.exc {
                    self.scan(exc, scopes);
                }
                if let Some(cause) = &raise.cause {
                    self.scan(cause, scopes);
                }
            }
            ast::Stmt::Assert(assert) => {
                self.scan(&assert.test, scopes);
                if let Some(msg) = &assert.msg {
                    self.scan(msg, scopes);
                }
            }
            ast::Stmt::Delete(delete) => {
                for target in &delete.targets {
                    if let ast::Expr::Name(name) = target {
                        scopes.unbind(name.id.as_str());
                    } else {
                        self.scan(target, scopes);
                    }
                }
            }
            ast::Stmt::If(inner) => {
                self.scan(&inner.test, scopes);
                let mut then_branch = scopes.fork();
                self.walk_stmts(&inner.body, &mut then_branch);
                if inner.orelse.is_empty() {
                    scopes.join(vec![then_branch], true);
                } else {
                    let mut else_branch = scopes.fork();
                    self.walk_stmts(&inner.orelse, &mut else_branch);
                    scopes.join(vec![then_branch, else_branch], false);
                }
            }
            ast::Stmt::While(inner) => {
                self.scan(&inner.test, scopes);
                self.walk_loop_body(None, &inner.body, scopes);
                self.walk_stmts(&inner.orelse, scopes);
            }
            ast::Stmt::For(inner) => {
                self.scan(&inner.iter, scopes);
                self.walk_loop_body(Some(&inner.target), &inner.body, scopes);
                self.walk_stmts(&inner.orelse, scopes);
            }
            ast::Stmt::AsyncFor(inner) => {
                self.scan(&inner.iter, scopes);
                self.walk_loop_body(Some(&inner.target), &inner.body, scopes);
                self.walk_stmts(&inner.orelse, scopes);
            }
            ast::Stmt::With(inner) => self.walk_with(&inner.items, &inner.body, scopes),
            ast::Stmt::AsyncWith(inner) => self.walk_with(&inner.items, &inner.body, scopes),
            ast::Stmt::Try(inner) => {
                self.walk_try(
                    &inner.body,
                    &inner.handlers,
                    &inner.orelse,
                    &inner.finalbody,
                    scopes,
                );
            }
            ast::Stmt::TryStar(inner) => {
                self.walk_try(
                    &inner.body,
                    &inner.handlers,
                    &inner.orelse,
                    &inner.finalbody,
                    scopes,
                );
            }
            ast::Stmt::Match(inner) => {
                self.scan(&inner.subject, scopes);
                let mut branches = Vec::new();
                for case in &inner.cases {
                    let mut branch = scopes.fork();
                    let mut captured = BTreeSet::new();
                    super::names::collect_pattern_names(&case.pattern, &mut captured);
                    for name in captured {
                        branch.bind(&name, Binding::LocalVar(LocalValue::Opaque));
                    }
                    if let Some(guard) = &case.guard {
                        self.scan(guard, &mut branch);
                    }
                    self.walk_stmts(&case.body, &mut branch);
                    branches.push(branch);
                }
                scopes.join(branches, true);
            }
            ast::Stmt::Global(inner) => {
                for name in &inner.names {
                    scopes.bind(name.as_str(), Binding::Unknown);
                }
            }
            ast::Stmt::Nonlocal(inner) => {
                for name in &inner.names {
                    scopes.bind(name.as_str(), Binding::Unknown);
                }
            }
            ast::Stmt::TypeAlias(_)
            | ast::Stmt::Pass(_)
            | ast::Stmt::Break(_)
            | ast::Stmt::Continue(_) => {}
        }
    }

    fn walk_with(
        &mut self,
        items: &[ast::WithItem],
        body: &[ast::Stmt],
        scopes: &mut ScopeStack<'_>,
    ) {
        for item in items {
            self.scan(&item.context_expr, scopes);
            if let Some(vars) = &item.optional_vars {
                bind_target(vars, &Binding::LocalVar(LocalValue::Opaque), scopes);
            }
        }
        self.walk_stmts(body, scopes);
    }

    fn walk_try(
        &mut self,
        body: &[ast::Stmt],
        handlers: &[ast::ExceptHandler],
        orelse: &[ast::Stmt],
        finalbody: &[ast::Stmt],
        scopes: &mut ScopeStack<'_>,
    ) {
        let mut main = scopes.fork();
        self.walk_stmts(body, &mut main);
        self.walk_stmts(orelse, &mut main);
        let mut branches = vec![main];
        for handler in handlers {
            let ast::ExceptHandler::ExceptHandler(handler) = handler;
            let mut branch = scopes.fork();
            if let Some(kind) = &handler.type_ {
                self.scan(kind, &mut branch);
            }
            if let Some(name) = &handler.name {
                branch.bind(name.as_str(), Binding::LocalVar(LocalValue::Opaque));
            }
            self.walk_stmts(&handler.body, &mut branch);
            branches.push(branch);
        }
        // The pre-try state is a possible path (the body may raise at any
        // point), so include it in the join: conservative, never confident.
        scopes.join(branches, true);
        self.walk_stmts(finalbody, scopes);
    }

    fn walk_loop_body(
        &mut self,
        target: Option<&ast::Expr>,
        body: &[ast::Stmt],
        scopes: &mut ScopeStack<'_>,
    ) {
        let mut iteration = scopes.fork();
        // A use inside the body may see a value from a previous iteration;
        // widen every loop-assigned name first, then let straight-line
        // assignments inside the body re-establish precise bindings.
        for name in assigned_names(body) {
            iteration.bind(&name, Binding::Unknown);
        }
        if let Some(target) = target {
            bind_target(
                target,
                &Binding::LocalVar(LocalValue::Opaque),
                &mut iteration,
            );
        }
        self.walk_stmts(body, &mut iteration);
        // The loop may run zero times: join with the pre-loop state.
        scopes.join(vec![iteration], true);
    }

    fn walk_function_def(
        &mut self,
        name: &ast::Identifier,
        args: &ast::Arguments,
        body: &[ast::Stmt],
        decorators: &[ast::Expr],
        scopes: &mut ScopeStack<'_>,
    ) {
        for decorator in decorators {
            self.scan(decorator, scopes);
        }
        let qualified = self.qualified(name.as_str());
        scopes.bind(name.as_str(), Binding::LocalFunction(qualified));
        if self.mode == WalkMode::Bind {
            return;
        }
        // Default expressions are evaluated in the enclosing scope.
        for arg in args
            .posonlyargs
            .iter()
            .chain(&args.args)
            .chain(&args.kwonlyargs)
        {
            if let Some(default) = &arg.default {
                self.scan(default, scopes);
            }
        }
        self.walk_function_body(name.as_str(), args, body, None, scopes);
    }

    /// Walks a function body. `method_of` is the qualified class id when the
    /// function is a method defined directly in a class body.
    fn walk_function_body(
        &mut self,
        name: &str,
        args: &ast::Arguments,
        body: &[ast::Stmt],
        method_of: Option<&str>,
        scopes: &mut ScopeStack<'_>,
    ) {
        self.function_stack.push(name.to_owned());
        let previous_method_class = self.method_class.clone();
        if let Some(class) = method_of {
            self.method_class = Some(class.to_owned());
        }
        let nested = self.function_stack.len() > 1;
        if nested {
            // Closures see the enclosing function frames.
            scopes.push_frame();
            bind_params(args, method_of, scopes);
            self.walk_stmts(body, scopes);
            scopes.pop_frame();
        } else {
            // Top-level functions and methods see the final module namespace.
            let mut fresh = ScopeStack::new(self.final_ns, self.module_unknown_star);
            bind_params(args, method_of, &mut fresh);
            self.walk_stmts(body, &mut fresh);
        }
        self.method_class = previous_method_class;
        self.function_stack.pop();
    }

    fn walk_class_def(&mut self, def: &ast::StmtClassDef, scopes: &mut ScopeStack<'_>) {
        for decorator in &def.decorator_list {
            self.scan(decorator, scopes);
        }
        let qualified = self.qualified(def.name.as_str());
        let in_function = !self.function_stack.is_empty();
        scopes.bind(def.name.as_str(), Binding::LocalClass(qualified.clone()));

        if self.mode == WalkMode::Bind && !in_function {
            let bases = def
                .bases
                .iter()
                .map(|base| self.resolve_base(base, scopes))
                .collect();
            let methods = direct_methods(&def.body);
            self.class_records.push(ClassRecord {
                qualified_name: qualified.clone(),
                name: def.name.to_string(),
                file: self.file,
                range: def.range(),
                bases,
                methods,
                kinds: BTreeSet::new(),
                reached_bases: BTreeSet::new(),
                bases_fully_resolved: false,
            });
        }
        for keyword in &def.keywords {
            self.scan(&keyword.value, scopes);
        }

        // Walk the class body with a class-scope frame; methods are walked
        // afterwards because Python method bodies do not see class scope.
        self.class_stack.push(qualified.clone());
        scopes.push_frame();
        let mut deferred: Vec<&ast::Stmt> = Vec::new();
        for stmt in &def.body {
            match stmt {
                ast::Stmt::FunctionDef(method) => {
                    let method_qualified = self.qualified(method.name.as_str());
                    scopes.bind(
                        method.name.as_str(),
                        Binding::LocalFunction(method_qualified),
                    );
                    deferred.push(stmt);
                }
                ast::Stmt::AsyncFunctionDef(method) => {
                    let method_qualified = self.qualified(method.name.as_str());
                    scopes.bind(
                        method.name.as_str(),
                        Binding::LocalFunction(method_qualified),
                    );
                    deferred.push(stmt);
                }
                other => self.walk_stmt(other, scopes),
            }
        }
        scopes.pop_frame();

        if self.mode == WalkMode::Full {
            for stmt in deferred {
                let (name, args, body, decorators) = match stmt {
                    ast::Stmt::FunctionDef(method) => (
                        method.name.as_str(),
                        method.args.as_ref(),
                        method.body.as_slice(),
                        method.decorator_list.as_slice(),
                    ),
                    ast::Stmt::AsyncFunctionDef(method) => (
                        method.name.as_str(),
                        method.args.as_ref(),
                        method.body.as_slice(),
                        method.decorator_list.as_slice(),
                    ),
                    _ => continue,
                };
                for decorator in decorators {
                    self.scan(decorator, scopes);
                }
                for arg in args
                    .posonlyargs
                    .iter()
                    .chain(&args.args)
                    .chain(&args.kwonlyargs)
                {
                    if let Some(default) = &arg.default {
                        self.scan(default, scopes);
                    }
                }
                self.walk_function_body(name, args, body, Some(&qualified), scopes);
            }
        }
        self.class_stack.pop();
    }

    fn resolve_base(&self, base: &ast::Expr, scopes: &ScopeStack<'_>) -> BaseRef {
        let unresolved = || BaseRef::Unresolved(self.source.slice(base.range()).to_owned());
        let Some((root, segments)) = flatten_dotted(base) else {
            return unresolved();
        };
        if segments.is_empty() {
            return match scopes.lookup(&root) {
                Some(Binding::LocalClass(id) | Binding::ImportedSymbol(id)) => {
                    BaseRef::Resolved(id)
                }
                _ => unresolved(),
            };
        }
        match scopes.lookup(&root) {
            Some(Binding::ImportedModule(module)) => {
                match self.resolver.module_attr(self.spaces, &module, &segments) {
                    Binding::ImportedSymbol(id) | Binding::LocalClass(id) => BaseRef::Resolved(id),
                    _ => unresolved(),
                }
            }
            _ => unresolved(),
        }
    }

    // -- expressions and call facts ---------------------------------------

    /// Visits an expression when collecting facts; returns the fact index
    /// when the expression itself is a call. No-op in bind mode.
    fn scan(&mut self, expr: &ast::Expr, scopes: &mut ScopeStack<'_>) -> Option<usize> {
        if self.mode == WalkMode::Bind {
            return None;
        }
        self.visit_expr(expr, scopes)
    }

    #[allow(clippy::too_many_lines, reason = "one arm per expression kind")]
    fn visit_expr(&mut self, expr: &ast::Expr, scopes: &mut ScopeStack<'_>) -> Option<usize> {
        match expr {
            ast::Expr::Call(call) => Some(self.visit_call(call, scopes)),
            ast::Expr::BoolOp(inner) => {
                for value in &inner.values {
                    self.visit_expr(value, scopes);
                }
                None
            }
            ast::Expr::NamedExpr(inner) => {
                self.visit_expr(&inner.value, scopes);
                let bound = self.classify_value(&inner.value, scopes);
                bind_target(&inner.target, &bound, scopes);
                None
            }
            ast::Expr::BinOp(inner) => {
                self.visit_expr(&inner.left, scopes);
                self.visit_expr(&inner.right, scopes);
                None
            }
            ast::Expr::UnaryOp(inner) => {
                self.visit_expr(&inner.operand, scopes);
                None
            }
            ast::Expr::Lambda(inner) => {
                scopes.push_frame();
                bind_params(&inner.args, None, scopes);
                self.visit_expr(&inner.body, scopes);
                scopes.pop_frame();
                None
            }
            ast::Expr::IfExp(inner) => {
                self.visit_expr(&inner.test, scopes);
                self.visit_expr(&inner.body, scopes);
                self.visit_expr(&inner.orelse, scopes);
                None
            }
            ast::Expr::Dict(inner) => {
                for key in inner.keys.iter().flatten() {
                    self.visit_expr(key, scopes);
                }
                for value in &inner.values {
                    self.visit_expr(value, scopes);
                }
                None
            }
            ast::Expr::Set(inner) => {
                for element in &inner.elts {
                    self.visit_expr(element, scopes);
                }
                None
            }
            ast::Expr::ListComp(inner) => {
                self.visit_comprehension(&inner.generators, &[&inner.elt], scopes);
                None
            }
            ast::Expr::SetComp(inner) => {
                self.visit_comprehension(&inner.generators, &[&inner.elt], scopes);
                None
            }
            ast::Expr::DictComp(inner) => {
                self.visit_comprehension(&inner.generators, &[&inner.key, &inner.value], scopes);
                None
            }
            ast::Expr::GeneratorExp(inner) => {
                self.visit_comprehension(&inner.generators, &[&inner.elt], scopes);
                None
            }
            ast::Expr::Await(inner) => self.visit_expr(&inner.value, scopes),
            ast::Expr::Yield(inner) => {
                if let Some(value) = &inner.value {
                    self.visit_expr(value, scopes);
                }
                None
            }
            ast::Expr::YieldFrom(inner) => self.visit_expr(&inner.value, scopes),
            ast::Expr::Compare(inner) => {
                self.visit_expr(&inner.left, scopes);
                for comparator in &inner.comparators {
                    self.visit_expr(comparator, scopes);
                }
                None
            }
            ast::Expr::FormattedValue(inner) => {
                self.visit_expr(&inner.value, scopes);
                None
            }
            ast::Expr::JoinedStr(inner) => {
                for value in &inner.values {
                    self.visit_expr(value, scopes);
                }
                None
            }
            ast::Expr::Attribute(inner) => {
                self.visit_expr(&inner.value, scopes);
                None
            }
            ast::Expr::Subscript(inner) => {
                self.visit_expr(&inner.value, scopes);
                self.visit_expr(&inner.slice, scopes);
                None
            }
            ast::Expr::Starred(inner) => {
                self.visit_expr(&inner.value, scopes);
                None
            }
            ast::Expr::List(inner) => {
                for element in &inner.elts {
                    self.visit_expr(element, scopes);
                }
                None
            }
            ast::Expr::Tuple(inner) => {
                for element in &inner.elts {
                    self.visit_expr(element, scopes);
                }
                None
            }
            ast::Expr::Slice(inner) => {
                for part in [&inner.lower, &inner.upper, &inner.step]
                    .into_iter()
                    .flatten()
                {
                    self.visit_expr(part, scopes);
                }
                None
            }
            ast::Expr::Constant(_) | ast::Expr::Name(_) => None,
        }
    }

    fn visit_comprehension(
        &mut self,
        generators: &[ast::Comprehension],
        elements: &[&ast::Expr],
        scopes: &mut ScopeStack<'_>,
    ) {
        scopes.push_frame();
        for generator in generators {
            self.visit_expr(&generator.iter, scopes);
            bind_target(
                &generator.target,
                &Binding::LocalVar(LocalValue::Opaque),
                scopes,
            );
            for condition in &generator.ifs {
                self.visit_expr(condition, scopes);
            }
        }
        for element in elements {
            self.visit_expr(element, scopes);
        }
        scopes.pop_frame();
    }

    fn visit_call(&mut self, call: &ast::ExprCall, scopes: &mut ScopeStack<'_>) -> usize {
        // Python evaluates the callee before the arguments; resolve the
        // callee first so a walrus in an argument cannot leak backwards.
        self.visit_expr(&call.func, scopes);
        let (receiver, candidates) = self.resolve_callee(&call.func, scopes);

        let mut arguments = Vec::new();
        let mut positional_count = 0;
        for arg in &call.args {
            positional_count += 1;
            let shape = if let ast::Expr::Starred(starred) = arg {
                self.visit_expr(&starred.value, scopes);
                ArgShape::Other
            } else {
                self.argument_shape(arg, scopes)
            };
            arguments.push(CallArgument {
                keyword: None,
                shape,
                range: arg.range(),
            });
        }
        let mut keyword_names = BTreeSet::new();
        for keyword in &call.keywords {
            let shape = self.argument_shape(&keyword.value, scopes);
            let name = keyword.arg.as_ref().map(std::string::ToString::to_string);
            if let Some(name) = &name {
                keyword_names.insert(name.clone());
            }
            arguments.push(CallArgument {
                keyword: name,
                shape,
                range: keyword.value.range(),
            });
        }

        let fact = QualifiedCall {
            file: self.file,
            call_range: call.range(),
            callee_range: call.func.range(),
            candidates,
            receiver,
            positional_count,
            keyword_names,
            arguments,
            context: self.context(),
        };
        self.facts.push(fact);
        self.facts.len() - 1
    }

    fn argument_shape(&mut self, expr: &ast::Expr, scopes: &mut ScopeStack<'_>) -> ArgShape {
        if let Some(index) = self.visit_expr(expr, scopes) {
            return ArgShape::Call(index);
        }
        match expr {
            ast::Expr::Constant(_) | ast::Expr::JoinedStr(_) => ArgShape::Literal,
            ast::Expr::Name(_) => ArgShape::Name,
            ast::Expr::Lambda(_) => ArgShape::Lambda,
            _ => ArgShape::Other,
        }
    }

    fn resolve_callee(
        &self,
        callee: &ast::Expr,
        scopes: &ScopeStack<'_>,
    ) -> (ReceiverKind, BTreeSet<String>) {
        match callee {
            ast::Expr::Name(name) => match scopes.lookup(name.id.as_str()) {
                Some(
                    Binding::ImportedSymbol(id)
                    | Binding::LocalClass(id)
                    | Binding::LocalFunction(id),
                ) => (ReceiverKind::Direct, BTreeSet::from([id])),
                Some(Binding::Unknown) => (ReceiverKind::Unknown, BTreeSet::new()),
                _ => (ReceiverKind::Direct, BTreeSet::new()),
            },
            ast::Expr::Attribute(_) => {
                let Some((root, segments)) = flatten_dotted(callee) else {
                    return (ReceiverKind::Unknown, BTreeSet::new());
                };
                self.resolve_attribute_callee(&root, &segments, scopes)
            }
            _ => (ReceiverKind::Unknown, BTreeSet::new()),
        }
    }

    fn resolve_attribute_callee(
        &self,
        root: &str,
        segments: &[String],
        scopes: &ScopeStack<'_>,
    ) -> (ReceiverKind, BTreeSet<String>) {
        match scopes.lookup(root) {
            Some(Binding::ImportedModule(module)) => {
                let candidates = match self.resolver.module_attr(self.spaces, &module, segments) {
                    Binding::ImportedSymbol(id)
                    | Binding::LocalClass(id)
                    | Binding::LocalFunction(id) => BTreeSet::from([id]),
                    _ => BTreeSet::new(),
                };
                (ReceiverKind::ModuleAlias, candidates)
            }
            Some(Binding::LocalVar(LocalValue::Instance(kind))) if segments.len() == 1 => {
                let method = &segments[0];
                let candidates = match self.index {
                    Some(index) => index.method_candidates(&kind, method),
                    None => BTreeSet::from([format!("{kind}.{method}")]),
                };
                let is_self_scene = root == "self"
                    && self.method_class.as_deref() == Some(kind.as_str())
                    && self.index.is_some_and(|index| index.is_scene_class(&kind));
                let receiver = if is_self_scene {
                    ReceiverKind::SelfScene
                } else {
                    ReceiverKind::KnownInstance(kind)
                };
                (receiver, candidates)
            }
            Some(Binding::ImportedSymbol(id)) if segments.len() == 1 => (
                ReceiverKind::Direct,
                BTreeSet::from([format!("{id}.{}", segments[0])]),
            ),
            Some(Binding::LocalClass(id)) if segments.len() == 1 => {
                let candidates = match self.index {
                    Some(index) => index.method_candidates(&id, &segments[0]),
                    None => BTreeSet::from([format!("{id}.{}", segments[0])]),
                };
                (ReceiverKind::Direct, candidates)
            }
            _ => (ReceiverKind::Unknown, BTreeSet::new()),
        }
    }

    /// Classifies the value of an assignment for straight-line tracking.
    fn classify_value(&self, expr: &ast::Expr, scopes: &ScopeStack<'_>) -> Binding {
        match expr {
            ast::Expr::Name(name) => scopes
                .lookup(name.id.as_str())
                .unwrap_or(Binding::LocalVar(LocalValue::Opaque)),
            ast::Expr::Attribute(_) => match flatten_dotted(expr) {
                Some((root, segments)) => match scopes.lookup(&root) {
                    Some(Binding::ImportedModule(module)) => {
                        self.resolver.module_attr(self.spaces, &module, &segments)
                    }
                    _ => Binding::LocalVar(LocalValue::Opaque),
                },
                None => Binding::LocalVar(LocalValue::Opaque),
            },
            ast::Expr::Call(call) => match self.classify_value(&call.func, scopes) {
                Binding::ImportedSymbol(id) | Binding::LocalClass(id) => {
                    Binding::LocalVar(LocalValue::Instance(id))
                }
                Binding::Unknown => Binding::Unknown,
                _ => Binding::LocalVar(LocalValue::Opaque),
            },
            _ => Binding::LocalVar(LocalValue::Opaque),
        }
    }

    fn expand_star(&mut self, module: Option<&str>, scopes: &mut ScopeStack<'_>) {
        match module {
            Some("manim") => {
                for (name, canonical) in &self.resolver.surface.star_exports {
                    scopes.bind(name, Binding::ImportedSymbol(canonical.clone()));
                }
            }
            Some(source) if !source.starts_with("manim.") && self.spaces.module_exists(source) => {
                match self.spaces.export_names(source) {
                    Some(names) => {
                        for name in names {
                            let bound = self
                                .spaces
                                .binding(source, &name)
                                .unwrap_or(Binding::Unknown);
                            scopes.bind(&name, bound);
                        }
                        if self.spaces.star_incomplete(source) {
                            scopes.set_unknown_star();
                        }
                    }
                    None => scopes.set_unknown_star(),
                }
            }
            // `manim.*` submodule stars, external packages, and unresolved
            // relative stars all widen the namespace to Unknown.
            _ => scopes.set_unknown_star(),
        }
    }
}

fn bind_params(args: &ast::Arguments, method_of: Option<&str>, scopes: &mut ScopeStack<'_>) {
    let mut positional = args.posonlyargs.iter().chain(&args.args);
    if let Some(class) = method_of {
        if let Some(first) = positional.next() {
            if first.def.arg.as_str() == "self" {
                scopes.bind(
                    "self",
                    Binding::LocalVar(LocalValue::Instance(class.to_owned())),
                );
            } else {
                scopes.bind(
                    first.def.arg.as_str(),
                    Binding::LocalVar(LocalValue::Opaque),
                );
            }
        }
    }
    for arg in positional {
        scopes.bind(arg.def.arg.as_str(), Binding::LocalVar(LocalValue::Opaque));
    }
    for arg in &args.kwonlyargs {
        scopes.bind(arg.def.arg.as_str(), Binding::LocalVar(LocalValue::Opaque));
    }
    if let Some(vararg) = &args.vararg {
        scopes.bind(vararg.arg.as_str(), Binding::LocalVar(LocalValue::Opaque));
    }
    if let Some(kwarg) = &args.kwarg {
        scopes.bind(kwarg.arg.as_str(), Binding::LocalVar(LocalValue::Opaque));
    }
}

fn bind_target(target: &ast::Expr, value: &Binding, scopes: &mut ScopeStack<'_>) {
    match target {
        ast::Expr::Name(name) => scopes.bind(name.id.as_str(), value.clone()),
        ast::Expr::Tuple(tuple) => {
            for element in &tuple.elts {
                bind_target(element, &Binding::LocalVar(LocalValue::Opaque), scopes);
            }
        }
        ast::Expr::List(list) => {
            for element in &list.elts {
                bind_target(element, &Binding::LocalVar(LocalValue::Opaque), scopes);
            }
        }
        ast::Expr::Starred(starred) => {
            bind_target(
                &starred.value,
                &Binding::LocalVar(LocalValue::Opaque),
                scopes,
            );
        }
        // Attribute / subscript targets do not bind local names.
        _ => {}
    }
}

fn direct_methods(body: &[ast::Stmt]) -> BTreeMap<String, TextRange> {
    let mut methods = BTreeMap::new();
    for stmt in body {
        match stmt {
            ast::Stmt::FunctionDef(def) => {
                methods.insert(def.name.to_string(), def.range());
            }
            ast::Stmt::AsyncFunctionDef(def) => {
                methods.insert(def.name.to_string(), def.range());
            }
            _ => {}
        }
    }
    methods
}

fn literal_all(expr: &ast::Expr) -> Option<BTreeSet<String>> {
    let elements = match expr {
        ast::Expr::List(list) => &list.elts,
        ast::Expr::Tuple(tuple) => &tuple.elts,
        _ => return None,
    };
    let mut names = BTreeSet::new();
    for element in elements {
        let ast::Expr::Constant(constant) = element else {
            return None;
        };
        let ast::Constant::Str(name) = &constant.value else {
            return None;
        };
        names.insert(name.clone());
    }
    Some(names)
}

// ---------------------------------------------------------------------------
// Index construction.
// ---------------------------------------------------------------------------

#[allow(
    clippy::too_many_lines,
    reason = "linear pipeline: module tree, fixpoint, records, roles"
)]
fn build_index(
    parsed: &[ParsedModule<'_>],
    source_roots: &[String],
    surface: &ManimSurface,
) -> ProjectIndex {
    // 1. Module tree.
    let mut module_of_file = BTreeMap::new();
    let mut owners: BTreeMap<String, ParsedModule<'_>> = BTreeMap::new();
    let mut module_tree = BTreeSet::new();
    for module in parsed {
        let identity = module_identity(module.source.relative_path(), source_roots);
        // First file wins a module-name collision (load order).
        owners.entry(identity.name.clone()).or_insert(*module);
        for prefix in dotted_prefixes(&identity.name) {
            module_tree.insert(prefix);
        }
        module_of_file.insert(module.file, identity);
    }

    // 2. Namespace fixpoint over star-import and re-export chains.
    let empty_ns = BTreeMap::new();
    let mut states: BTreeMap<String, ModuleState> = BTreeMap::new();
    let mut class_records: Vec<ClassRecord> = Vec::new();
    let max_passes = owners.len() + 2;
    for _ in 0..max_passes {
        let snapshot = states.clone();
        let spaces = SnapshotSpaces {
            states: &snapshot,
            tree: &module_tree,
        };
        let mut next: BTreeMap<String, ModuleState> = BTreeMap::new();
        let mut records: Vec<ClassRecord> = Vec::new();
        for (name, module) in &owners {
            let identity = &module_of_file[&module.file];
            let mut walker = Walker {
                resolver: Resolver { surface },
                spaces: &spaces,
                identity,
                source: module.source,
                file: module.file,
                mode: WalkMode::Bind,
                index: None,
                final_ns: &empty_ns,
                module_unknown_star: false,
                facts: Vec::new(),
                class_records: Vec::new(),
                all_names: None,
                all_invalid: false,
                class_stack: Vec::new(),
                function_stack: Vec::new(),
                method_class: None,
            };
            let mut scopes = ScopeStack::new(&empty_ns, false);
            walker.walk_stmts(&module.ast.body, &mut scopes);
            let (namespace, has_unknown_star) = scopes.into_root_frame();
            let exports = export_surface(
                &namespace,
                has_unknown_star,
                walker.all_names.clone(),
                walker.all_invalid,
            );
            records.append(&mut walker.class_records);
            next.insert(
                name.clone(),
                ModuleState {
                    namespace,
                    has_unknown_star,
                    exports,
                },
            );
        }
        let stable = next == states;
        states = next;
        class_records = records;
        if stable {
            break;
        }
    }

    // 3. Assemble module records.
    let mut modules = BTreeMap::new();
    for (name, module) in &owners {
        let state = &states[name];
        let identity = &module_of_file[&module.file];
        modules.insert(
            name.clone(),
            ModuleRecord {
                file: module.file,
                name: name.clone(),
                is_package: identity.is_package,
                namespace: state.namespace.clone(),
                has_unknown_star: state.has_unknown_star,
                exports: state.exports.clone(),
            },
        );
    }

    // 4. Class hierarchy and Manim subclass discovery.
    let mut classes: BTreeMap<String, ClassRecord> = BTreeMap::new();
    for record in class_records {
        classes.insert(record.qualified_name.clone(), record);
    }
    compute_roles(&mut classes, surface);

    let mut index = ProjectIndex {
        modules,
        module_of_file,
        module_tree,
        classes,
        scene_classes: BTreeSet::new(),
        mobject_classes: BTreeSet::new(),
        animation_classes: BTreeSet::new(),
    };
    for (name, class) in &index.classes {
        if class.kinds.contains(&ManimKind::Scene) {
            index.scene_classes.insert(name.clone());
        }
        if class.kinds.contains(&ManimKind::Mobject) {
            index.mobject_classes.insert(name.clone());
        }
        if class.kinds.contains(&ManimKind::Animation) {
            index.animation_classes.insert(name.clone());
        }
    }
    index
}

fn dotted_prefixes(name: &str) -> Vec<String> {
    let mut prefixes = Vec::new();
    let mut current = String::new();
    for segment in name.split('.') {
        if !current.is_empty() {
            current.push('.');
        }
        current.push_str(segment);
        prefixes.push(current.clone());
    }
    prefixes
}

fn export_surface(
    namespace: &BTreeMap<String, Binding>,
    has_unknown_star: bool,
    all_names: Option<BTreeSet<String>>,
    all_invalid: bool,
) -> ExportSurface {
    if !all_invalid {
        if let Some(names) = all_names {
            return ExportSurface {
                names,
                from_literal_all: true,
                complete: true,
            };
        }
    }
    ExportSurface {
        names: namespace
            .keys()
            .filter(|name| !name.starts_with('_'))
            .cloned()
            .collect(),
        from_literal_all: false,
        complete: !has_unknown_star && !all_invalid,
    }
}

fn compute_roles(classes: &mut BTreeMap<String, ClassRecord>, surface: &ManimSurface) {
    let bases_view: BTreeMap<String, Vec<BaseRef>> = classes
        .iter()
        .map(|(name, class)| (name.clone(), class.bases.clone()))
        .collect();
    let mut memo: BTreeMap<String, (BTreeSet<String>, bool)> = BTreeMap::new();
    let names: Vec<String> = classes.keys().cloned().collect();
    for name in &names {
        let mut visiting = BTreeSet::new();
        reach(name, &bases_view, &mut memo, &mut visiting);
    }
    for (name, class) in classes {
        let (reached, fully) = memo.get(name).cloned().unwrap_or_default();
        let mut kinds = BTreeSet::new();
        if reached
            .iter()
            .any(|base| surface.scene_bases.contains(base))
        {
            kinds.insert(ManimKind::Scene);
        }
        if reached
            .iter()
            .any(|base| surface.mobject_bases.contains(base))
        {
            kinds.insert(ManimKind::Mobject);
        }
        if reached
            .iter()
            .any(|base| surface.animation_bases.contains(base))
        {
            kinds.insert(ManimKind::Animation);
        }
        class.kinds = kinds;
        class.reached_bases = reached;
        class.bases_fully_resolved = fully;
    }
}

fn reach(
    name: &str,
    bases_view: &BTreeMap<String, Vec<BaseRef>>,
    memo: &mut BTreeMap<String, (BTreeSet<String>, bool)>,
    visiting: &mut BTreeSet<String>,
) -> (BTreeSet<String>, bool) {
    if let Some(cached) = memo.get(name) {
        return cached.clone();
    }
    if !visiting.insert(name.to_owned()) {
        // Inheritance cycle: contributes nothing, marks the chain unresolved.
        return (BTreeSet::new(), false);
    }
    let mut reached = BTreeSet::new();
    let mut fully = true;
    if let Some(bases) = bases_view.get(name) {
        for base in bases {
            match base {
                BaseRef::Resolved(id) if bases_view.contains_key(id) => {
                    let (sub, sub_fully) = reach(id, bases_view, memo, visiting);
                    reached.extend(sub);
                    fully &= sub_fully;
                }
                BaseRef::Resolved(id) => {
                    reached.insert(id.clone());
                }
                BaseRef::Unresolved(_) => fully = false,
            }
        }
    }
    visiting.remove(name);
    memo.insert(name.to_owned(), (reached.clone(), fully));
    (reached, fully)
}

// ---------------------------------------------------------------------------
// Call-fact collection.
// ---------------------------------------------------------------------------

fn collect_calls(
    parsed: &[ParsedModule<'_>],
    index: &ProjectIndex,
    surface: &ManimSurface,
) -> QualifiedCallFacts {
    let spaces = IndexSpaces { index };
    let empty_ns = BTreeMap::new();
    let mut calls: Vec<QualifiedCall> = Vec::new();
    for module in parsed {
        let Some(identity) = index.module_of_file.get(&module.file) else {
            continue;
        };
        let record = index.modules.get(&identity.name);
        let (final_ns, module_unknown_star) = match record {
            // Only the collision winner has a record; a loser file still
            // gets walked, over an empty namespace.
            Some(record) if record.file == module.file => {
                (&record.namespace, record.has_unknown_star)
            }
            _ => (&empty_ns, false),
        };
        let mut walker = Walker {
            resolver: Resolver { surface },
            spaces: &spaces,
            identity,
            source: module.source,
            file: module.file,
            mode: WalkMode::Full,
            index: Some(index),
            final_ns,
            module_unknown_star,
            facts: Vec::new(),
            class_records: Vec::new(),
            all_names: None,
            all_invalid: false,
            class_stack: Vec::new(),
            function_stack: Vec::new(),
            method_class: None,
        };
        let mut scopes = ScopeStack::new(&empty_ns, false);
        walker.walk_stmts(&module.ast.body, &mut scopes);
        let offset = calls.len();
        for mut fact in walker.facts {
            for argument in &mut fact.arguments {
                if let ArgShape::Call(child) = argument.shape {
                    argument.shape = ArgShape::Call(child + offset);
                }
            }
            calls.push(fact);
        }
    }
    QualifiedCallFacts { calls }
}
