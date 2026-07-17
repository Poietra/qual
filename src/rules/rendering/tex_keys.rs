//! `MLR127`: a literal `get_part_by_tex` / `set_color_by_tex` key that
//! provably cannot occur in the receiver's literal MathTex/Tex arguments.
//!
//! Soundness: `MathTex` splits its arguments into parts (double-brace
//! groups, `substrings_to_isolate`, `tex_to_color_map` keys), and every
//! part is a contiguous substring of exactly one constructor argument —
//! parts never span arguments and splitting only shrinks them. Both
//! by-tex lookups match keys against part tex strings, so a key that is
//! not a substring of *any* literal argument can never match, regardless
//! of isolation kwargs. Only that absent-substring case fires; a key that
//! merely fails to line up with the current split (but does occur in the
//! literal) stays silent, because isolation could make it match.
//!
//! Receiver binding is proven conservatively: the receiver must be a
//! plain name bound exactly once in its file, directly to a
//! knowledge-resolved `MathTex(...)` / `Tex(...)` call whose positional
//! arguments are all plain string literals. Everything else is silence.

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::ast::{self, Ranged};
use rustpython_parser::text_size::TextRange;
use serde_json::json;

use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::frontend::index::LiteralFact;
use crate::rules::base::{Rule, RuleContext};
use crate::source::{FileId, SourceFile};

/// Method names the rule judges (exact-match part lookups in the target
/// Manim source; upstream 0.20 uses substring matching — the
/// absent-substring test is sound under both).
const BY_TEX_METHODS: &[&str] = &["get_part_by_tex", "set_color_by_tex"];

const MLR127: RuleMetadata = RuleMetadata {
    id: "MLR127",
    summary: "Literal by-tex key cannot occur in the MathTex literal",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 2,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

pub(super) struct UnmatchableTexKey;

/// Per-file binding facts: name → literal constructor arguments, for
/// names bound exactly once (by the tracked assignment itself).
struct FileBindings {
    tex_literals: BTreeMap<String, TexBinding>,
}

/// One proven `name = MathTex(...)` binding.
struct TexBinding {
    /// Canonical constructor id (`...MathTex` / `...Tex`).
    constructor: String,
    /// The literal positional argument strings, in order.
    parts: Vec<String>,
}

impl Rule for UnmatchableTexKey {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR127
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        if context.knowledge().is_none() {
            return Vec::new();
        }
        let facts = context.qualified_calls();
        let profiles = context.config().active_profile_names();
        let mut per_file: BTreeMap<FileId, FileBindings> = BTreeMap::new();
        let mut diagnostics = Vec::new();
        for call in &facts.calls {
            // A by-tex method call on a plain-name receiver?
            let file = context.sources().file(call.file);
            let callee = file.slice(call.callee_range);
            let Some((receiver, method)) = callee.rsplit_once('.') else {
                continue;
            };
            if !BY_TEX_METHODS.contains(&method) || !is_identifier(receiver) {
                continue;
            }
            let bindings = per_file
                .entry(call.file)
                .or_insert_with(|| file_bindings(context, file));
            let Some(binding) = bindings.tex_literals.get(receiver) else {
                continue;
            };
            let key_argument = match call.keyword("tex") {
                Some(argument) => argument,
                None if !call.has_star_args => match call.positional(0) {
                    Some(argument) if argument.keyword.is_none() => argument,
                    _ => continue,
                },
                None => continue,
            };
            let Some(LiteralFact::Str {
                value: key,
                prefix,
                range,
            }) = &key_argument.literal
            else {
                continue;
            };
            if prefix.bytes || key.is_empty() {
                continue;
            }
            if binding.parts.iter().any(|part| part.contains(key.as_str())) {
                // The key occurs in the literal; isolation could make it
                // a matchable part, so this is not provably dead.
                continue;
            }
            let constructor = super::short_name(&binding.constructor);
            let mut evidence = BTreeMap::new();
            evidence.insert("constructor".to_owned(), json!(binding.constructor));
            evidence.insert("key".to_owned(), json!(key));
            evidence.insert("tex_arguments".to_owned(), json!(binding.parts));
            diagnostics.push(super::build_diagnostic(
                &MLR127,
                file,
                *range,
                Confidence::High,
                format!(
                    "`.{method}({key:?})` can never match: {key:?} does not occur \
                     in the `{constructor}` literal arguments of `{receiver}`"
                ),
                "MathTex matches by-tex keys against its split parts, and every \
                 part is a substring of one constructor argument — so a key that \
                 is absent from all literal arguments cannot match any part. \
                 get_part_by_tex then returns None and set_color_by_tex silently \
                 changes nothing. Pass a substring that occurs in the TeX source, \
                 and isolate it as its own part with double braces \
                 ({{ ... }}) or substrings_to_isolate=[...] so the lookup can \
                 select it.",
                evidence,
                profiles.clone(),
                None,
            ));
        }
        diagnostics
    }
}

/// Whether `text` is one plain Python identifier (no dots, calls, or
/// spaces), so `text.method(...)` really reads a name binding.
fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_alphanumeric() || ch == '_')
}

/// Collects the file's proven `name = MathTex/Tex(literals...)` bindings.
fn file_bindings(context: &RuleContext<'_>, file: &SourceFile) -> FileBindings {
    let profile = context
        .knowledge()
        .expect("caller checked the profile exists");
    let facts = context.qualified_calls();

    // 1. Knowledge-resolved MathTex / Tex constructions whose positional
    //    arguments are all plain string literals.
    let mut constructors: BTreeMap<(u32, u32), TexBinding> = BTreeMap::new();
    for call in facts.calls_in_file(file.id()) {
        let Some((constructor_id, _)) = super::single_knowledge_symbol(profile, &call.candidates)
        else {
            continue;
        };
        if !super::TEX_CONSTRUCTORS.contains(&constructor_id) || call.has_star_args {
            continue;
        }
        let mut parts = Vec::new();
        let mut all_literal = true;
        for argument in call
            .arguments
            .iter()
            .take(call.positional_count)
            .filter(|argument| argument.keyword.is_none())
        {
            match &argument.literal {
                Some(LiteralFact::Str { value, prefix, .. }) if !prefix.bytes => {
                    parts.push(value.clone());
                }
                _ => {
                    all_literal = false;
                    break;
                }
            }
        }
        if !all_literal || parts.is_empty() {
            continue;
        }
        constructors.insert(
            (call.call_range.start().into(), call.call_range.end().into()),
            TexBinding {
                constructor: constructor_id.to_owned(),
                parts,
            },
        );
    }

    // 2. Names assigned directly to such a construction, bound exactly
    //    once in the whole file (the assignment itself included).
    let counts = binding_counts(file);
    let mut tex_literals = BTreeMap::new();
    if let Some(module) = file.ast() {
        super::each_statement(&module.body, &mut |stmt| {
            let ast::Stmt::Assign(assign) = stmt else {
                return;
            };
            let [ast::Expr::Name(name)] = assign.targets.as_slice() else {
                return;
            };
            let value_range: TextRange = assign.value.range();
            let key = (u32::from(value_range.start()), u32::from(value_range.end()));
            if counts.get(name.id.as_str()).copied() != Some(1) {
                return;
            }
            if let Some(binding) = constructors.remove(&key) {
                tex_literals.insert(name.id.to_string(), binding);
            }
        });
    }
    FileBindings { tex_literals }
}

/// How many times each name is bound anywhere in the file (assignments,
/// loop and `with` targets, defs, classes, imports, parameters; `global`
/// / `nonlocal` count as bindings to disqualify cross-scope rebinding).
fn binding_counts(file: &SourceFile) -> BTreeMap<String, usize> {
    let mut bound: Vec<BTreeSet<String>> = Vec::new();
    let Some(module) = file.ast() else {
        return BTreeMap::new();
    };
    super::each_statement(&module.body, &mut |stmt| {
        let mut names = BTreeSet::new();
        match stmt {
            ast::Stmt::Assign(assign) => {
                for target in &assign.targets {
                    super::collect_target_names(target, &mut names);
                }
            }
            ast::Stmt::AnnAssign(assign) => super::collect_target_names(&assign.target, &mut names),
            ast::Stmt::AugAssign(assign) => super::collect_target_names(&assign.target, &mut names),
            ast::Stmt::For(inner) => super::collect_target_names(&inner.target, &mut names),
            ast::Stmt::AsyncFor(inner) => super::collect_target_names(&inner.target, &mut names),
            ast::Stmt::With(inner) => {
                for item in &inner.items {
                    if let Some(vars) = &item.optional_vars {
                        super::collect_target_names(vars, &mut names);
                    }
                }
            }
            ast::Stmt::AsyncWith(inner) => {
                for item in &inner.items {
                    if let Some(vars) = &item.optional_vars {
                        super::collect_target_names(vars, &mut names);
                    }
                }
            }
            ast::Stmt::FunctionDef(def) => {
                names.insert(def.name.to_string());
                super::collect_parameter_names(&def.args, &mut names);
            }
            ast::Stmt::AsyncFunctionDef(def) => {
                names.insert(def.name.to_string());
                super::collect_parameter_names(&def.args, &mut names);
            }
            ast::Stmt::ClassDef(def) => {
                names.insert(def.name.to_string());
            }
            ast::Stmt::Import(import) => {
                for alias in &import.names {
                    names.insert(alias.asname.as_ref().map_or_else(
                        || {
                            alias
                                .name
                                .split('.')
                                .next()
                                .unwrap_or(alias.name.as_str())
                                .to_owned()
                        },
                        std::string::ToString::to_string,
                    ));
                }
            }
            ast::Stmt::ImportFrom(import) => {
                for alias in &import.names {
                    names.insert(
                        alias.asname.as_ref().map_or_else(
                            || alias.name.to_string(),
                            std::string::ToString::to_string,
                        ),
                    );
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
        bound.push(names);
    });
    let mut counts = BTreeMap::new();
    for names in bound {
        for name in names {
            *counts.entry(name).or_insert(0) += 1;
        }
    }
    counts
}
