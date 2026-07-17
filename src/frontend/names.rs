//! Scope-aware binding resolution (DESIGN §5.3).
//!
//! Tracks what each name refers to at a given lexical position: module
//! scope, class scope, and function scopes with local assignment shadowing.
//! Where the binding at a use site is not determinable (conditional rebinds,
//! loop-carried values, unresolved star imports) it is an explicit
//! [`Binding::Unknown`], never a guess.

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::ast;

/// The value a plain local variable holds, as far as trivial straight-line
/// tracking can tell.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum LocalValue {
    /// The variable was assigned an instantiation of a known class
    /// (`sq = Square()`); carries the canonical or project-qualified class id.
    Instance(String),
    /// The variable holds some value the frontend does not model.
    Opaque,
}

/// What a name refers to at one lexical position.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Binding {
    /// A symbol imported from another module, carrying its canonical
    /// qualified id (e.g. `manim.mobject.geometry.polygram.Square`).
    ImportedSymbol(String),
    /// A module object (`import manim as mn` binds `mn` to `manim`).
    ImportedModule(String),
    /// A class defined in the project, carrying its qualified id.
    LocalClass(String),
    /// A function defined in the project, carrying its qualified id.
    LocalFunction(String),
    /// A local variable with an optionally tracked value.
    LocalVar(LocalValue),
    /// Explicitly unknown: dynamic constructs, unresolved star imports, or
    /// conflicting bindings after a control-flow join.
    Unknown,
}

/// One lexical scope frame (module, class body, or function body).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeFrame {
    bindings: BTreeMap<String, Binding>,
}

/// A stack of lexical scopes over an immutable base namespace.
///
/// The base is the enclosing module's final namespace when walking function
/// bodies (Python functions see the module state at call time), or empty when
/// walking module-level code sequentially.
#[derive(Debug, Clone)]
pub struct ScopeStack<'a> {
    base: &'a BTreeMap<String, Binding>,
    /// When true, a name miss resolves to [`Binding::Unknown`] instead of
    /// "definitely unbound" because an unresolved star import may bind it.
    unknown_star: bool,
    frames: Vec<ScopeFrame>,
}

impl<'a> ScopeStack<'a> {
    /// Creates a stack over `base` with one empty frame pushed.
    #[must_use]
    pub fn new(base: &'a BTreeMap<String, Binding>, unknown_star: bool) -> Self {
        Self {
            base,
            unknown_star,
            frames: vec![ScopeFrame::default()],
        }
    }

    /// Marks the whole stack as shadowed by an unresolved star import.
    pub fn set_unknown_star(&mut self) {
        self.unknown_star = true;
    }

    /// Whether an unresolved star import widens missing names to Unknown.
    #[must_use]
    pub const fn has_unknown_star(&self) -> bool {
        self.unknown_star
    }

    /// Pushes a fresh innermost frame.
    pub fn push_frame(&mut self) {
        self.frames.push(ScopeFrame::default());
    }

    /// Pops the innermost frame.
    pub fn pop_frame(&mut self) {
        self.frames.pop();
    }

    /// Binds `name` in the innermost frame.
    pub fn bind(&mut self, name: &str, binding: Binding) {
        if let Some(frame) = self.frames.last_mut() {
            frame.bindings.insert(name.to_owned(), binding);
        }
    }

    /// Removes `name` from the innermost frame (a `del` statement).
    pub fn unbind(&mut self, name: &str) {
        if let Some(frame) = self.frames.last_mut() {
            frame.bindings.remove(name);
        }
    }

    /// Resolves `name` at the current position.
    ///
    /// `None` means "definitely not bound here" (e.g. a builtin);
    /// `Some(Binding::Unknown)` means the binding exists but is not
    /// determinable.
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<Binding> {
        for frame in self.frames.iter().rev() {
            if let Some(binding) = frame.bindings.get(name) {
                return Some(binding.clone());
            }
        }
        if let Some(binding) = self.base.get(name) {
            return Some(binding.clone());
        }
        if self.unknown_star {
            return Some(Binding::Unknown);
        }
        None
    }

    /// Clones the current state to explore one control-flow branch.
    #[must_use]
    pub fn fork(&self) -> Self {
        self.clone()
    }

    /// Consumes the stack and returns the root frame's bindings together
    /// with the unknown-star flag. Used to turn a sequential module-body walk
    /// into the module's final namespace.
    #[must_use]
    pub fn into_root_frame(mut self) -> (BTreeMap<String, Binding>, bool) {
        let bindings = if self.frames.is_empty() {
            BTreeMap::new()
        } else {
            self.frames.remove(0).bindings
        };
        (bindings, self.unknown_star)
    }

    /// Joins branch end states back into `self`.
    ///
    /// When `include_self` is true the pre-branch state of `self` is one of
    /// the joined paths (a fallthrough, e.g. an `if` without `else`). Any
    /// name whose binding differs between joined paths becomes
    /// [`Binding::Unknown`]; agreeing bindings are kept.
    pub fn join(&mut self, branches: Vec<Self>, include_self: bool) {
        let mut states: Vec<Vec<ScopeFrame>> = Vec::new();
        if include_self {
            states.push(self.frames.clone());
        }
        let mut unknown_star = self.unknown_star;
        for branch in branches {
            unknown_star |= branch.unknown_star;
            states.push(branch.frames);
        }
        self.unknown_star = unknown_star;
        let Some(first) = states.first() else {
            return;
        };
        let depth = first.len().min(self.frames.len());
        for level in 0..depth {
            let mut names: BTreeSet<String> = BTreeSet::new();
            for state in &states {
                if let Some(frame) = state.get(level) {
                    names.extend(frame.bindings.keys().cloned());
                }
            }
            for name in names {
                let mut agreed: Option<Option<&Binding>> = None;
                let mut conflict = false;
                for state in &states {
                    let value = state.get(level).and_then(|frame| frame.bindings.get(&name));
                    match &agreed {
                        None => agreed = Some(value),
                        Some(previous) if *previous == value => {}
                        Some(_) => {
                            conflict = true;
                            break;
                        }
                    }
                }
                let frame = &mut self.frames[level];
                if conflict {
                    frame.bindings.insert(name, Binding::Unknown);
                } else if let Some(Some(binding)) = agreed {
                    frame.bindings.insert(name, binding.clone());
                } else {
                    frame.bindings.remove(&name);
                }
            }
        }
    }
}

/// Collects every name a statement list may (re)bind, without descending
/// into nested function or class bodies.
///
/// Used to widen loop-carried names to Unknown before walking a loop body:
/// a use inside the body may see either the pre-loop value or a value from a
/// previous iteration.
#[must_use]
pub fn assigned_names(stmts: &[ast::Stmt]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    collect_assigned(stmts, &mut names);
    names
}

#[allow(clippy::too_many_lines, reason = "one arm per statement kind")]
fn collect_assigned(stmts: &[ast::Stmt], names: &mut BTreeSet<String>) {
    for stmt in stmts {
        match stmt {
            ast::Stmt::Assign(assign) => {
                for target in &assign.targets {
                    collect_target_names(target, names);
                }
            }
            ast::Stmt::AnnAssign(assign) => {
                if assign.value.is_some() {
                    collect_target_names(&assign.target, names);
                }
            }
            ast::Stmt::AugAssign(assign) => collect_target_names(&assign.target, names),
            ast::Stmt::For(inner) => {
                collect_target_names(&inner.target, names);
                collect_assigned(&inner.body, names);
                collect_assigned(&inner.orelse, names);
            }
            ast::Stmt::AsyncFor(inner) => {
                collect_target_names(&inner.target, names);
                collect_assigned(&inner.body, names);
                collect_assigned(&inner.orelse, names);
            }
            ast::Stmt::While(inner) => {
                collect_assigned(&inner.body, names);
                collect_assigned(&inner.orelse, names);
            }
            ast::Stmt::If(inner) => {
                collect_assigned(&inner.body, names);
                collect_assigned(&inner.orelse, names);
            }
            ast::Stmt::With(inner) => {
                for item in &inner.items {
                    if let Some(vars) = &item.optional_vars {
                        collect_target_names(vars, names);
                    }
                }
                collect_assigned(&inner.body, names);
            }
            ast::Stmt::AsyncWith(inner) => {
                for item in &inner.items {
                    if let Some(vars) = &item.optional_vars {
                        collect_target_names(vars, names);
                    }
                }
                collect_assigned(&inner.body, names);
            }
            ast::Stmt::Try(inner) => {
                collect_assigned(&inner.body, names);
                collect_assigned(&inner.orelse, names);
                collect_assigned(&inner.finalbody, names);
                for handler in &inner.handlers {
                    let ast::ExceptHandler::ExceptHandler(handler) = handler;
                    if let Some(name) = &handler.name {
                        names.insert(name.to_string());
                    }
                    collect_assigned(&handler.body, names);
                }
            }
            ast::Stmt::TryStar(inner) => {
                collect_assigned(&inner.body, names);
                collect_assigned(&inner.orelse, names);
                collect_assigned(&inner.finalbody, names);
                for handler in &inner.handlers {
                    let ast::ExceptHandler::ExceptHandler(handler) = handler;
                    if let Some(name) = &handler.name {
                        names.insert(name.to_string());
                    }
                    collect_assigned(&handler.body, names);
                }
            }
            ast::Stmt::Match(inner) => {
                for case in &inner.cases {
                    collect_pattern_names(&case.pattern, names);
                    collect_assigned(&case.body, names);
                }
            }
            ast::Stmt::Import(import) => {
                for binding in super::imports::import_bindings(import) {
                    names.insert(binding.name);
                }
            }
            ast::Stmt::ImportFrom(import) => {
                for alias in &import.names {
                    if alias.name.as_str() == "*" {
                        continue;
                    }
                    let local = alias.asname.as_ref().unwrap_or(&alias.name);
                    names.insert(local.to_string());
                }
            }
            ast::Stmt::FunctionDef(def) => {
                names.insert(def.name.to_string());
            }
            ast::Stmt::AsyncFunctionDef(def) => {
                names.insert(def.name.to_string());
            }
            ast::Stmt::ClassDef(def) => {
                names.insert(def.name.to_string());
            }
            _ => {}
        }
    }
}

/// Collects the plain names bound by an assignment / loop target expression.
pub fn collect_target_names(target: &ast::Expr, names: &mut BTreeSet<String>) {
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
        // Attribute / subscript targets do not bind local names.
        _ => {}
    }
}

/// Collects names captured by a `match` pattern.
pub fn collect_pattern_names(pattern: &ast::Pattern, names: &mut BTreeSet<String>) {
    match pattern {
        ast::Pattern::MatchValue(_) | ast::Pattern::MatchSingleton(_) => {}
        ast::Pattern::MatchSequence(inner) => {
            for pattern in &inner.patterns {
                collect_pattern_names(pattern, names);
            }
        }
        ast::Pattern::MatchMapping(inner) => {
            for pattern in &inner.patterns {
                collect_pattern_names(pattern, names);
            }
            if let Some(rest) = &inner.rest {
                names.insert(rest.to_string());
            }
        }
        ast::Pattern::MatchClass(inner) => {
            for pattern in inner.patterns.iter().chain(&inner.kwd_patterns) {
                collect_pattern_names(pattern, names);
            }
        }
        ast::Pattern::MatchStar(inner) => {
            if let Some(name) = &inner.name {
                names.insert(name.to_string());
            }
        }
        ast::Pattern::MatchAs(inner) => {
            if let Some(pattern) = &inner.pattern {
                collect_pattern_names(pattern, names);
            }
            if let Some(name) = &inner.name {
                names.insert(name.to_string());
            }
        }
        ast::Pattern::MatchOr(inner) => {
            for pattern in &inner.patterns {
                collect_pattern_names(pattern, names);
            }
        }
    }
}

/// Flattens an attribute chain rooted at a plain name into
/// `(root, [segment, ...])`, e.g. `mn.animation.FadeIn` →
/// `("mn", ["animation", "FadeIn"])`. Returns `None` for anything rooted at
/// a call, subscript, or other dynamic expression.
#[must_use]
pub fn flatten_dotted(expr: &ast::Expr) -> Option<(String, Vec<String>)> {
    match expr {
        ast::Expr::Name(name) => Some((name.id.to_string(), Vec::new())),
        ast::Expr::Attribute(attribute) => {
            let (root, mut segments) = flatten_dotted(&attribute.value)?;
            segments.push(attribute.attr.to_string());
            Some((root, segments))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_base() -> BTreeMap<String, Binding> {
        BTreeMap::new()
    }

    #[test]
    fn innermost_frame_shadows_outer_and_base() {
        let mut base = empty_base();
        base.insert("x".to_owned(), Binding::ImportedModule("manim".to_owned()));
        let mut scopes = ScopeStack::new(&base, false);
        assert_eq!(
            scopes.lookup("x"),
            Some(Binding::ImportedModule("manim".to_owned()))
        );
        scopes.bind("x", Binding::LocalVar(LocalValue::Opaque));
        scopes.push_frame();
        scopes.bind("x", Binding::Unknown);
        assert_eq!(scopes.lookup("x"), Some(Binding::Unknown));
        scopes.pop_frame();
        assert_eq!(
            scopes.lookup("x"),
            Some(Binding::LocalVar(LocalValue::Opaque))
        );
    }

    #[test]
    fn miss_with_unknown_star_is_unknown_not_unbound() {
        let base = empty_base();
        let scopes = ScopeStack::new(&base, true);
        assert_eq!(scopes.lookup("anything"), Some(Binding::Unknown));
        let strict = ScopeStack::new(&base, false);
        assert_eq!(strict.lookup("anything"), None);
    }

    #[test]
    fn join_keeps_agreement_and_widens_conflicts() {
        let base = empty_base();
        let mut scopes = ScopeStack::new(&base, false);
        scopes.bind("kept", Binding::LocalVar(LocalValue::Opaque));

        let mut then_branch = scopes.fork();
        then_branch.bind("x", Binding::LocalClass("m.A".to_owned()));
        let mut else_branch = scopes.fork();
        else_branch.bind("x", Binding::LocalClass("m.B".to_owned()));

        scopes.join(vec![then_branch, else_branch], false);
        assert_eq!(scopes.lookup("x"), Some(Binding::Unknown));
        assert_eq!(
            scopes.lookup("kept"),
            Some(Binding::LocalVar(LocalValue::Opaque))
        );
    }

    #[test]
    fn join_with_fallthrough_widens_names_bound_in_only_one_branch() {
        let base = empty_base();
        let mut scopes = ScopeStack::new(&base, false);
        let mut branch = scopes.fork();
        branch.bind("x", Binding::LocalClass("m.A".to_owned()));
        scopes.join(vec![branch], true);
        assert_eq!(scopes.lookup("x"), Some(Binding::Unknown));
    }
}
