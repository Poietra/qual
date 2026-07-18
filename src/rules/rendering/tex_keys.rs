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
//! plain name bound exactly once in its file (frontend binding facts),
//! directly to a knowledge-resolved `MathTex(...)` / `Tex(...)` call whose
//! positional arguments are all plain string literals (frontend statement
//! facts: the construction is the whole assignment RHS). Everything else
//! is silence.

use std::collections::BTreeMap;

use serde_json::json;

use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::frontend::index::LiteralFact;
use crate::frontend::statements::StatementRole;
use crate::rules::base::{Rule, RuleContext};
use crate::source::FileId;

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
            let Some(parts) = call.callee_dotted.as_deref() else {
                continue;
            };
            let [receiver, method] = parts else {
                continue;
            };
            if !BY_TEX_METHODS.contains(&method.as_str()) {
                continue;
            }
            let file = context.sources().file(call.file);
            let bindings = per_file
                .entry(call.file)
                .or_insert_with(|| file_bindings(context, call.file));
            let Some(binding) = bindings.tex_literals.get(receiver.as_str()) else {
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

/// Collects the file's proven `name = MathTex/Tex(literals...)` bindings:
/// knowledge-resolved constructions with all-literal positional arguments
/// that form the whole RHS of a single-name assignment (statement facts)
/// whose name is bound by exactly one statement in the file (binding
/// facts).
fn file_bindings(context: &RuleContext<'_>, file: FileId) -> FileBindings {
    let profile = context
        .knowledge()
        .expect("caller checked the profile exists");
    let facts = context.qualified_calls();
    let mut tex_literals = BTreeMap::new();
    let Some(bindings) = context.binding_facts().file(file) else {
        return FileBindings { tex_literals };
    };
    for call in facts.calls_in_file(file) {
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
        // The construction must be the whole RHS of `name = MathTex(...)`,
        // and `name` must be bound by exactly one statement in the file
        // (the assignment itself).
        let Some(StatementRole::AssignmentRhs { target: Some(name) }) = context
            .statement_facts()
            .for_call(call)
            .map(|statement| &statement.role)
        else {
            continue;
        };
        if bindings.binding_statement_count(name) != 1 {
            continue;
        }
        tex_literals.insert(
            name.clone(),
            TexBinding {
                constructor: constructor_id.to_owned(),
                parts,
            },
        );
    }
    FileBindings { tex_literals }
}
