//! Family membership rules: `MLC126`, `MLC127`.
//!
//! Manim's family invariants (DESIGN §3.4): `Mobject.add` accepts only
//! mobjects and ignores duplicate children with a warning, and a
//! `VMobject` family (including `VGroup`) must contain only vectorized
//! mobjects — heterogeneous collections need `Group`.

use std::collections::BTreeMap;

use rustpython_parser::text_size::TextRange;
use serde_json::Value;

use crate::diagnostic::{
    Confidence, Diagnostic, Fix, FixApplicability, RuleMetadata, Severity, TextEdit,
};
use crate::frontend::index::{
    ArgShape, CallArgument, LiteralFact, ProjectIndex, QualifiedCall, ReceiverKind,
};
use crate::knowledge::{KnowledgeProfile, SymbolKind};
use crate::rules::base::{Rule, RuleContext};
use crate::source::SourceFile;

use super::support::{
    ClassKind, GROUP, MOBJECT_ADD, VGROUP, build_diagnostic, candidates_value, classify_class,
    conclusive_target, receiver_conclusive,
};

/// A call confirmed to build or extend a Manim family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Container {
    /// `.add(...)` on an instance of a vectorized (`VMobject`) family.
    VectorizedAdd,
    /// `.add(...)` on an instance of a plain (non-vectorized) mobject.
    PlainAdd,
    /// A `VGroup(...)` constructor call.
    VGroupCtor,
    /// A `Group(...)` constructor call.
    GroupCtor,
}

impl Container {
    /// Human-readable description of the call for messages.
    const fn describe(self) -> &'static str {
        match self {
            Self::VectorizedAdd => "VMobject-family `.add()`",
            Self::PlainAdd => "Mobject `.add()`",
            Self::VGroupCtor => "`VGroup(...)`",
            Self::GroupCtor => "`Group(...)`",
        }
    }

    /// Whether the family accepts only vectorized mobjects.
    const fn vectorized_only(self) -> bool {
        matches!(self, Self::VectorizedAdd | Self::VGroupCtor)
    }
}

/// Classifies a call as a confirmed family container, or `None`.
///
/// `Scene.add` resolves to `manim.scene.scene.Scene.add`, never to
/// [`MOBJECT_ADD`], so Scene membership calls are excluded here.
fn container_of(
    profile: &KnowledgeProfile,
    index: &ProjectIndex,
    call: &QualifiedCall,
) -> Option<Container> {
    let (canonical, _) = conclusive_target(profile, index, call)?;
    match canonical.as_str() {
        VGROUP => Some(Container::VGroupCtor),
        GROUP => Some(Container::GroupCtor),
        MOBJECT_ADD => {
            let ReceiverKind::KnownInstance(kind) = &call.receiver else {
                return None;
            };
            match classify_class(profile, index, kind)?.kind {
                ClassKind::Vmobject => Some(Container::VectorizedAdd),
                ClassKind::MobjectOnly => Some(Container::PlainAdd),
                ClassKind::Scene | ClassKind::Animation | ClassKind::KnownOther => None,
            }
        }
        _ => None,
    }
}

/// What a child argument's value is confirmed to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChildKind {
    /// Certainly a vectorized mobject: always a valid child.
    Vectorized,
    /// Certainly a mobject and certainly *not* vectorized (`ImageMobject`).
    PlainMobject,
    /// Certainly a mobject, vectorization unknown: valid for (a), silent
    /// for the vectorized-family check.
    MobjectUnknownVectorization,
    /// Certainly not a mobject of any kind (Animation, Scene, Camera, or a
    /// fully resolved non-Manim class).
    NonMobject,
}

/// Classifies one candidate class id of a child value; `None` is Unknown.
///
/// Curated `Function` / `Method` / `Constant` ids are `None` on purpose: a
/// kind candidate recorded from `x = some_function(...)` names the callee,
/// not the value's class, so nothing is known about the value
/// (e.g. `always_redraw` returns a mobject).
fn classify_child_id(
    profile: &KnowledgeProfile,
    index: &ProjectIndex,
    id: &str,
) -> Option<ChildKind> {
    if let Some(entry) = profile.symbol(id) {
        return match entry.kind {
            SymbolKind::Vmobject => Some(ChildKind::Vectorized),
            SymbolKind::Mobject => Some(ChildKind::PlainMobject),
            SymbolKind::Scene | SymbolKind::Animation | SymbolKind::Camera => {
                Some(ChildKind::NonMobject)
            }
            SymbolKind::Function | SymbolKind::Method | SymbolKind::Constant => None,
        };
    }
    let fact = classify_class(profile, index, id)?;
    match fact.kind {
        ClassKind::Vmobject => Some(ChildKind::Vectorized),
        ClassKind::MobjectOnly => Some(if fact.conclusive {
            ChildKind::PlainMobject
        } else {
            ChildKind::MobjectUnknownVectorization
        }),
        ClassKind::Scene | ClassKind::Animation | ClassKind::KnownOther => {
            // A negative "not a mobject" claim needs the full base chain.
            fact.conclusive.then_some(ChildKind::NonMobject)
        }
    }
}

/// Uniform child classification over a candidate id set; `None` when the
/// set is empty, mixed, or any candidate is unknown.
fn uniform_child_kind<'a>(
    profile: &KnowledgeProfile,
    index: &ProjectIndex,
    candidates: impl IntoIterator<Item = &'a String>,
) -> Option<ChildKind> {
    let mut agreed: Option<ChildKind> = None;
    for candidate in candidates {
        let current = classify_child_id(profile, index, candidate)?;
        match agreed {
            None => agreed = Some(current),
            Some(existing) if existing == current => {}
            Some(_) => return None,
        }
    }
    agreed
}

// ---------------------------------------------------------------------------
// MLC126: non-mobject child / non-VMobject child in a vectorized family.
// ---------------------------------------------------------------------------

/// Metadata for [`InvalidFamilyChild`].
pub const MLC126: RuleMetadata = RuleMetadata {
    id: "MLC126",
    summary: "Family child is not a Mobject, or not a VMobject in a VGroup",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::High,
    implementation_phase: 1,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

/// Non-mobject children of `add` / `Group` / `VGroup`, and plain-Mobject
/// children of vectorized families (DESIGN §7.1 `MLC126`).
pub struct InvalidFamilyChild;

impl Rule for InvalidFamilyChild {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC126
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let calls = context.qualified_calls();
        let mut diagnostics = Vec::new();
        for call in &calls.calls {
            let Some(container) = container_of(profile, index, call) else {
                continue;
            };
            let file = context.sources().file(call.file);
            for position in 0..call.positional_count {
                let argument = &call.arguments[position];
                let child = match &argument.shape {
                    ArgShape::Literal => literal_child_kind(argument),
                    ArgShape::Name => {
                        if argument.kind_candidates.is_empty() {
                            None
                        } else {
                            uniform_child_kind(profile, index, &argument.kind_candidates)
                        }
                    }
                    ArgShape::Call(inner_index) => {
                        let inner = &calls.calls[*inner_index];
                        if inner.candidates.is_empty() || !receiver_conclusive(inner, index) {
                            None
                        } else {
                            uniform_child_kind(profile, index, &inner.candidates)
                        }
                    }
                    _ => None,
                };
                let diagnostic = match child {
                    Some(ChildKind::NonMobject) => Some(non_mobject_diagnostic(
                        context, file, call, container, argument,
                    )),
                    Some(ChildKind::PlainMobject) if container.vectorized_only() => Some(
                        plain_in_vectorized_diagnostic(context, file, call, container, argument),
                    ),
                    _ => None,
                };
                diagnostics.extend(diagnostic);
            }
        }
        diagnostics
    }
}

/// Child kind of a literal argument; `None` literals stay silent because
/// the catalog scopes (a) to numeric / string / boolean literals.
fn literal_child_kind(argument: &CallArgument) -> Option<ChildKind> {
    match argument.literal {
        Some(
            LiteralFact::Int(_)
            | LiteralFact::Float(_)
            | LiteralFact::Str { .. }
            | LiteralFact::Bool(_),
        ) => Some(ChildKind::NonMobject),
        _ => None,
    }
}

fn non_mobject_diagnostic(
    context: &RuleContext<'_>,
    file: &SourceFile,
    call: &QualifiedCall,
    container: Container,
    argument: &CallArgument,
) -> Diagnostic {
    let text = file.slice(argument.range);
    let mut evidence = child_evidence(call, argument, "non-mobject");
    evidence.insert(
        "container".to_owned(),
        Value::String(container.describe().to_owned()),
    );
    build_diagnostic(
        &MLC126,
        context,
        file,
        argument.range,
        format!(
            "Remove `{text}` from this {container} call: only Mobject instances \
             can join a Manim family, and this value is known not to be one.",
            container = container.describe()
        ),
        "`Mobject.add` (and the `Group` / `VGroup` constructors, which call it) \
         validates every child and raises `TypeError` for values that are not \
         Mobject instances; the family is built at construction time, so this \
         fails before anything is rendered (DESIGN §3.4)."
            .to_owned(),
        evidence,
    )
}

fn plain_in_vectorized_diagnostic(
    context: &RuleContext<'_>,
    file: &SourceFile,
    call: &QualifiedCall,
    container: Container,
    argument: &CallArgument,
) -> Diagnostic {
    let text = file.slice(argument.range);
    let mut evidence = child_evidence(call, argument, "non-vmobject-in-vectorized-family");
    evidence.insert(
        "container".to_owned(),
        Value::String(container.describe().to_owned()),
    );
    build_diagnostic(
        &MLC126,
        context,
        file,
        argument.range,
        format!(
            "Use `Group(...)` for `{text}`: this child is a plain Mobject (not \
             vectorized), and a {container} family accepts only VMobject \
             instances.",
            container = container.describe()
        ),
        "A `VMobject` family stores Bézier point data for every member, so \
         `VGroup` and other vectorized containers reject plain mobjects such \
         as `ImageMobject` with `TypeError`. `Group` is the heterogeneous \
         container that accepts any Mobject (DESIGN §3.4)."
            .to_owned(),
        evidence,
    )
}

fn child_evidence(
    call: &QualifiedCall,
    argument: &CallArgument,
    kind: &str,
) -> BTreeMap<String, Value> {
    let mut evidence = BTreeMap::new();
    evidence.insert("candidates".to_owned(), candidates_value(call));
    evidence.insert("child_kind".to_owned(), Value::String(kind.to_owned()));
    if !argument.kind_candidates.is_empty() {
        evidence.insert(
            "child_candidates".to_owned(),
            Value::Array(
                argument
                    .kind_candidates
                    .iter()
                    .map(|id| Value::String(id.clone()))
                    .collect(),
            ),
        );
    }
    evidence
}

// ---------------------------------------------------------------------------
// MLC127: duplicate child in one add() / VGroup() / Group() call.
// ---------------------------------------------------------------------------

/// Metadata for [`DuplicateChild`].
pub const MLC127: RuleMetadata = RuleMetadata {
    id: "MLC127",
    summary: "Duplicate child passed twice in one add() / VGroup() / Group()",
    default_enabled: true,
    default_severity: Severity::Info,
    minimum_confidence: Confidence::Certain,
    implementation_phase: 1,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

/// The same name passed twice to one family call (DESIGN §7.1 `MLC127`).
pub struct DuplicateChild;

impl Rule for DuplicateChild {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC127
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let mut diagnostics = Vec::new();
        for call in &context.qualified_calls().calls {
            let Some(container) = container_of(profile, index, call) else {
                continue;
            };
            let file = context.sources().file(call.file);
            let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
            for position in 0..call.positional_count {
                let argument = &call.arguments[position];
                if argument.shape != ArgShape::Name {
                    continue;
                }
                let name = file.slice(argument.range);
                match seen.get(name) {
                    None => {
                        seen.insert(name, position);
                    }
                    Some(_) => {
                        diagnostics.push(duplicate_diagnostic(
                            context, file, call, container, position, name,
                        ));
                    }
                }
            }
        }
        diagnostics
    }
}

fn duplicate_diagnostic(
    context: &RuleContext<'_>,
    file: &SourceFile,
    call: &QualifiedCall,
    container: Container,
    position: usize,
    name: &str,
) -> Diagnostic {
    let argument = &call.arguments[position];
    let mut evidence = BTreeMap::new();
    evidence.insert("candidates".to_owned(), candidates_value(call));
    evidence.insert("duplicate_name".to_owned(), Value::String(name.to_owned()));
    let mut diagnostic = build_diagnostic(
        &MLC127,
        context,
        file,
        argument.range,
        format!(
            "Remove the duplicate `{name}` from this {container} call: Manim \
             warns and ignores repeated children of a single add.",
            container = container.describe()
        ),
        "`Mobject.add` skips children that are already in the family and only \
         emits a runtime warning, so the second occurrence has no effect; the \
         duplicate argument is dead weight and usually signals a typo \
         (DESIGN §3.4)."
            .to_owned(),
        evidence,
    );
    diagnostic.fix = removal_fix(file, call, position);
    diagnostic
}

/// Safe fix removing the duplicate argument together with its separating
/// comma. Only produced when the text between the previous argument and the
/// duplicate is exactly one comma plus whitespace, so the edit is provably
/// parse-preserving.
fn removal_fix(file: &SourceFile, call: &QualifiedCall, position: usize) -> Option<Fix> {
    let previous = call.arguments.get(position.checked_sub(1)?)?;
    let argument = &call.arguments[position];
    let gap_range = TextRange::new(previous.range.end(), argument.range.start());
    let gap = file.slice(gap_range);
    let mut commas = 0usize;
    for character in gap.chars() {
        match character {
            ',' => commas += 1,
            character if character.is_whitespace() => {}
            _ => return None,
        }
    }
    if commas != 1 {
        return None;
    }
    let removal = TextRange::new(previous.range.end(), argument.range.end());
    Some(Fix {
        applicability: FixApplicability::Safe,
        message: format!(
            "Remove the duplicate `{name}` argument",
            name = file.slice(argument.range)
        ),
        edits: vec![TextEdit {
            path: file.relative_path().to_owned(),
            span: file.span_of_range(removal),
            replacement: String::new(),
        }],
    })
}
