//! Play-argument rules: `MLC101`, `MLC102`, `MLC103`, `MLC109`, `MLC122`.
//!
//! `Scene.play` compiles its positional arguments (DESIGN §3.2): Animation
//! instances pass through, `.animate` builders become animations, and
//! everything else raises `TypeError` at render time. These rules confirm
//! the call target through the knowledge profile before firing.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::diagnostic::{
    Confidence, Diagnostic, Fix, FixApplicability, RuleMetadata, Severity, TextEdit,
};
use crate::frontend::index::{
    ArgShape, ProjectIndex, QualifiedCall, QualifiedCallFacts, ReceiverKind,
};
use crate::knowledge::{KnowledgeProfile, SymbolKind};
use crate::rules::base::{Rule, RuleContext};
use crate::source::SourceFile;

use super::support::{
    ANIMATION_GROUP, APPLY_METHOD, SCENE_PLAY, SUCCESSION, bound_receiver, build_diagnostic,
    candidates_value, classify_class, conclusive_target, owned_by_mobject, receiver_conclusive,
    resolve_symbol, simple_dotted,
};

/// Yields every call confirmed to target `Scene.play` on a bound receiver.
fn confirmed_play_calls<'a>(
    profile: &'a KnowledgeProfile,
    index: &'a ProjectIndex,
    calls: &'a QualifiedCallFacts,
) -> impl Iterator<Item = &'a QualifiedCall> {
    calls.calls.iter().filter(move |call| {
        bound_receiver(call)
            && conclusive_target(profile, index, call)
                .is_some_and(|(canonical, _)| canonical == SCENE_PLAY)
    })
}

// ---------------------------------------------------------------------------
// MLC101: Scene.play() with zero animations.
// ---------------------------------------------------------------------------

/// Metadata for [`EmptyPlay`].
pub const MLC101: RuleMetadata = RuleMetadata {
    id: "MLC101",
    summary: "Scene.play requires at least one animation",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::Certain,
    implementation_phase: 1,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

/// `self.play()` with zero positional arguments (DESIGN §7.1 `MLC101`).
pub struct EmptyPlay;

impl Rule for EmptyPlay {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC101
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let mut diagnostics = Vec::new();
        for call in confirmed_play_calls(profile, index, context.qualified_calls()) {
            // A `*args` splat may supply animations at runtime.
            if call.has_star_args || call.positional_count != 0 {
                continue;
            }
            let file = context.sources().file(call.file);
            let message = if call.keyword_names.is_empty() {
                "Pass at least one animation to `Scene.play()`; calling it with no \
                 arguments raises `ValueError` at render time."
                    .to_owned()
            } else {
                format!(
                    "Pass at least one animation to `Scene.play()`; keyword arguments \
                     alone ({keywords}) provide zero animations and the call raises \
                     `ValueError` at render time.",
                    keywords = keyword_list(call)
                )
            };
            let explanation = "`Scene.play` compiles its positional arguments into \
                animations before rendering; play kwargs such as `run_time` only \
                configure animations that were passed, so an argument-less play has \
                nothing to render."
                .to_owned();
            let mut evidence = BTreeMap::new();
            evidence.insert("resolved".to_owned(), Value::String(SCENE_PLAY.to_owned()));
            evidence.insert("candidates".to_owned(), candidates_value(call));
            evidence.insert(
                "keyword_names".to_owned(),
                Value::Array(
                    call.keyword_names
                        .iter()
                        .map(|name| Value::String(name.clone()))
                        .collect(),
                ),
            );
            diagnostics.push(build_diagnostic(
                &MLC101,
                context,
                file,
                call.call_range,
                message,
                explanation,
                evidence,
            ));
        }
        diagnostics
    }
}

fn keyword_list(call: &QualifiedCall) -> String {
    call.keyword_names
        .iter()
        .map(|name| format!("`{name}=...`"))
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// Inner-call classification shared by MLC102 and MLC122.
// ---------------------------------------------------------------------------

/// What a call expression passed as an argument is confirmed to produce.
#[derive(Debug, Clone, PartialEq, Eq)]
enum InnerVerdict {
    /// A fluent mobject mutator: the call returns the (mutated) mobject
    /// itself. Carries the canonical method id.
    MethodResult(String),
    /// A mobject constructor: the call produces a bare mobject. Carries the
    /// canonical class id.
    Constructed(String),
    /// An Animation constructor: a valid play argument.
    Animation,
}

/// Classifies one candidate of an inner call; `None` means unknown.
fn classify_inner_candidate(
    profile: &KnowledgeProfile,
    index: &ProjectIndex,
    candidate: &str,
) -> Option<InnerVerdict> {
    if let Some((canonical, entry)) = resolve_symbol(profile, candidate) {
        return match entry.kind {
            SymbolKind::Method => {
                if entry.returns_self == Some(true) && owned_by_mobject(profile, index, &canonical)
                {
                    Some(InnerVerdict::MethodResult(canonical))
                } else {
                    None
                }
            }
            SymbolKind::Mobject | SymbolKind::Vmobject => {
                Some(InnerVerdict::Constructed(canonical))
            }
            SymbolKind::Animation => Some(InnerVerdict::Animation),
            SymbolKind::Scene
            | SymbolKind::Camera
            | SymbolKind::Function
            | SymbolKind::Constant => None,
        };
    }
    // A project class constructed directly (e.g. a custom Mobject subclass).
    let fact = classify_class(profile, index, candidate)?;
    if fact.kind == super::support::ClassKind::Animation {
        return Some(InnerVerdict::Animation);
    }
    if fact.conclusive_mobject() {
        return Some(InnerVerdict::Constructed(candidate.to_owned()));
    }
    None
}

/// Uniform verdict over every candidate of an inner call expression;
/// `None` when candidates are missing, mixed, or unknown.
fn inner_verdict(
    profile: &KnowledgeProfile,
    index: &ProjectIndex,
    file: &SourceFile,
    inner: &QualifiedCall,
) -> Option<InnerVerdict> {
    // `.animate` chains build animations; never classify through them.
    if file.slice(inner.callee_range).contains(".animate") {
        return None;
    }
    if inner.candidates.is_empty() || !receiver_conclusive(inner, index) {
        return None;
    }
    let mut verdict: Option<InnerVerdict> = None;
    for candidate in &inner.candidates {
        let current = classify_inner_candidate(profile, index, candidate)?;
        match &verdict {
            None => verdict = Some(current),
            Some(existing) if *existing == current => {}
            Some(_) => return None,
        }
    }
    verdict
}

// ---------------------------------------------------------------------------
// MLC102: play() argument not convertible to an Animation.
// ---------------------------------------------------------------------------

/// Metadata for [`NonAnimationPlayArgument`].
pub const MLC102: RuleMetadata = RuleMetadata {
    id: "MLC102",
    summary: "Scene.play argument cannot be converted to an Animation",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::High,
    implementation_phase: 1,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

/// `play(mob.shift(...))`, bare mobjects, and literals passed to `play`
/// (DESIGN §7.1 `MLC102`).
pub struct NonAnimationPlayArgument;

impl Rule for NonAnimationPlayArgument {
    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let calls = context.qualified_calls();
        let mut diagnostics = Vec::new();
        for call in confirmed_play_calls(profile, index, calls) {
            let file = context.sources().file(call.file);
            for position in 0..call.positional_count {
                let argument = &call.arguments[position];
                let diagnostic = match &argument.shape {
                    ArgShape::Literal => literal_argument(context, file, call, argument),
                    ArgShape::Name => name_argument(context, file, call, argument),
                    ArgShape::Call(inner_index) => {
                        let inner = &calls.calls[*inner_index];
                        call_argument(context, file, call, argument, inner)
                    }
                    _ => None,
                };
                diagnostics.extend(diagnostic);
            }
        }
        diagnostics
    }

    fn metadata(&self) -> &'static RuleMetadata {
        &MLC102
    }
}

fn literal_argument(
    context: &RuleContext<'_>,
    file: &SourceFile,
    call: &QualifiedCall,
    argument: &crate::frontend::index::CallArgument,
) -> Option<Diagnostic> {
    use crate::frontend::index::LiteralFact;
    let kind = match &argument.literal {
        Some(LiteralFact::Int(_) | LiteralFact::Float(_)) => "number",
        Some(LiteralFact::Str { .. }) => "string",
        _ => return None,
    };
    let text = file.slice(argument.range);
    let mut evidence = base_evidence(call);
    evidence.insert(
        "argument_kind".to_owned(),
        Value::String(format!("literal-{kind}")),
    );
    Some(build_diagnostic(
        &MLC102,
        context,
        file,
        argument.range,
        format!(
            "Remove the {kind} literal `{text}` from `Scene.play()`: play arguments \
                 must be animations or `.animate` builders."
        ),
        explanation_text(),
        evidence,
    ))
}

fn name_argument(
    context: &RuleContext<'_>,
    file: &SourceFile,
    call: &QualifiedCall,
    argument: &crate::frontend::index::CallArgument,
) -> Option<Diagnostic> {
    if argument.kind_candidates.is_empty() {
        return None;
    }
    let profile = context.knowledge()?;
    let index = context.project_index();
    let all_mobjects = argument.kind_candidates.iter().all(|kind| {
        classify_class(profile, index, kind)
            .is_some_and(super::support::ClassFact::conclusive_mobject)
    });
    if !all_mobjects {
        return None;
    }
    let text = file.slice(argument.range);
    let mut evidence = base_evidence(call);
    evidence.insert(
        "argument_kind".to_owned(),
        Value::String("bare-mobject".to_owned()),
    );
    evidence.insert(
        "kind_candidates".to_owned(),
        Value::Array(
            argument
                .kind_candidates
                .iter()
                .map(|kind| Value::String(kind.clone()))
                .collect(),
        ),
    );
    Some(build_diagnostic(
        &MLC102,
        context,
        file,
        argument.range,
        format!(
            "Wrap the bare mobject `{text}` in an animation (e.g. `Create({text})` \
                 or `FadeIn({text})`) before passing it to `Scene.play()`."
        ),
        explanation_text(),
        evidence,
    ))
}

fn call_argument(
    context: &RuleContext<'_>,
    file: &SourceFile,
    call: &QualifiedCall,
    argument: &crate::frontend::index::CallArgument,
    inner: &QualifiedCall,
) -> Option<Diagnostic> {
    let profile = context.knowledge()?;
    let index = context.project_index();
    match inner_verdict(profile, index, file, inner)? {
        InnerVerdict::Animation => None,
        InnerVerdict::MethodResult(canonical) => {
            let callee_text = file.slice(inner.callee_range);
            let mut evidence = base_evidence(call);
            evidence.insert(
                "argument_kind".to_owned(),
                Value::String("mobject-method-result".to_owned()),
            );
            evidence.insert("resolved_argument".to_owned(), Value::String(canonical));
            let mut diagnostic = build_diagnostic(
                &MLC102,
                context,
                file,
                argument.range,
                format!(
                    "`{callee_text}(...)` mutates the mobject immediately and returns \
                         the mobject itself, not an Animation; use \
                         `.animate` (e.g. `{fixed}(...)`) inside `Scene.play()`.",
                    fixed = animate_callee(callee_text)
                        .unwrap_or_else(|| format!("{callee_text} via .animate")),
                ),
                explanation_text(),
                evidence,
            );
            diagnostic.fix = animate_fix(file, inner, callee_text);
            Some(diagnostic)
        }
        InnerVerdict::Constructed(canonical) => {
            let text = file.slice(argument.range);
            let mut evidence = base_evidence(call);
            evidence.insert(
                "argument_kind".to_owned(),
                Value::String("constructed-mobject".to_owned()),
            );
            evidence.insert("resolved_argument".to_owned(), Value::String(canonical));
            Some(build_diagnostic(
                &MLC102,
                context,
                file,
                argument.range,
                format!(
                    "`{text}` constructs a bare mobject; wrap it in an animation \
                         (e.g. `Create(...)` or `FadeIn(...)`) before passing it to \
                         `Scene.play()`."
                ),
                explanation_text(),
                evidence,
            ))
        }
    }
}

fn base_evidence(call: &QualifiedCall) -> BTreeMap<String, Value> {
    let mut evidence = BTreeMap::new();
    evidence.insert("resolved".to_owned(), Value::String(SCENE_PLAY.to_owned()));
    evidence.insert("candidates".to_owned(), candidates_value(call));
    evidence
}

fn explanation_text() -> String {
    "`Scene.play` converts each positional argument during compilation: Animation \
     instances pass through and `.animate` builders become animations; every other \
     value raises `TypeError` at render time (DESIGN §3.2)."
        .to_owned()
}

/// `sq.shift` → `sq.animate.shift`, when the callee is a simple dotted name.
fn animate_callee(callee_text: &str) -> Option<String> {
    if !simple_dotted(callee_text) {
        return None;
    }
    let (base, method) = callee_text.rsplit_once('.')?;
    Some(format!("{base}.animate.{method}"))
}

/// Unsafe suggestion converting `mob.method(args)` to
/// `mob.animate.method(args)` for a single fluent call.
fn animate_fix(file: &SourceFile, inner: &QualifiedCall, callee_text: &str) -> Option<Fix> {
    if !matches!(inner.receiver, ReceiverKind::KnownInstance(_)) {
        return None;
    }
    let replacement = animate_callee(callee_text)?;
    Some(Fix {
        applicability: FixApplicability::Unsafe,
        message: format!("Replace `{callee_text}(...)` with `{replacement}(...)`"),
        edits: vec![TextEdit {
            path: file.relative_path().to_owned(),
            span: file.span_of_range(inner.callee_range),
            replacement,
        }],
    })
}

// ---------------------------------------------------------------------------
// MLC103: old bound-method play API.
// ---------------------------------------------------------------------------

/// Metadata for [`BoundMethodPlayArgument`].
pub const MLC103: RuleMetadata = RuleMetadata {
    id: "MLC103",
    summary: "Bound mobject method passed to Scene.play (pre-0.6 API)",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::Certain,
    implementation_phase: 1,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

/// `play(mob.shift, RIGHT)`: the removed method-based play API
/// (DESIGN §7.1 `MLC103`).
pub struct BoundMethodPlayArgument;

impl Rule for BoundMethodPlayArgument {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC103
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let mut diagnostics = Vec::new();
        for call in confirmed_play_calls(profile, index, context.qualified_calls()) {
            let file = context.sources().file(call.file);
            for position in 0..call.positional_count {
                let argument = &call.arguments[position];
                let ArgShape::Attribute(attribute) = &argument.shape else {
                    continue;
                };
                if attribute.candidates.is_empty() {
                    continue;
                }
                if !matches!(attribute.receiver, ReceiverKind::KnownInstance(_)) {
                    continue;
                }
                if let ReceiverKind::KnownInstance(kind) = &attribute.receiver {
                    if index
                        .classes
                        .get(kind)
                        .is_some_and(|record| !record.bases_fully_resolved)
                    {
                        continue;
                    }
                }
                let mut resolved: Option<String> = None;
                let mut conclusive = true;
                for candidate in &attribute.candidates {
                    let Some((canonical, entry)) = resolve_symbol(profile, candidate) else {
                        conclusive = false;
                        break;
                    };
                    let is_mobject_method = entry.kind == SymbolKind::Method
                        && (owned_by_mobject(profile, index, &canonical)
                            || entry.returns_self.is_some());
                    if !is_mobject_method {
                        conclusive = false;
                        break;
                    }
                    match &resolved {
                        None => resolved = Some(canonical),
                        Some(existing) if *existing == canonical => {}
                        Some(_) => {
                            conclusive = false;
                            break;
                        }
                    }
                }
                let Some(canonical) = resolved.filter(|_| conclusive) else {
                    continue;
                };
                let text = file.slice(argument.range);
                let mut evidence = BTreeMap::new();
                evidence.insert("resolved".to_owned(), Value::String(SCENE_PLAY.to_owned()));
                evidence.insert("resolved_argument".to_owned(), Value::String(canonical));
                diagnostics.push(build_diagnostic(
                    &MLC103,
                    context,
                    file,
                    argument.range,
                    format!(
                        "Replace the bound-method argument `{text}` with a `.animate` \
                         call: the pre-0.6 `play(mob.method, args)` API was removed and \
                         raises `TypeError` in Manim 0.20."
                    ),
                    "Passing a bound mobject method (plus its arguments) to `Scene.play` \
                     was the pre-community-0.6 animation API. Manim 0.20 only accepts \
                     Animation instances and `.animate` builders; a bare method object \
                     cannot be compiled into an animation (DESIGN §3.2)."
                        .to_owned(),
                    evidence,
                ));
            }
        }
        diagnostics
    }
}

// ---------------------------------------------------------------------------
// MLC109: empty AnimationGroup / Succession.
// ---------------------------------------------------------------------------

/// Metadata for [`EmptyAnimationGroup`].
pub const MLC109: RuleMetadata = RuleMetadata {
    id: "MLC109",
    summary: "AnimationGroup / Succession constructed without animations",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::Certain,
    implementation_phase: 1,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

/// `AnimationGroup()` / `Succession()` with zero animation arguments
/// (DESIGN §7.1 `MLC109`).
pub struct EmptyAnimationGroup;

impl Rule for EmptyAnimationGroup {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC109
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let mut diagnostics = Vec::new();
        for call in &context.qualified_calls().calls {
            if call.has_star_args || call.positional_count != 0 {
                continue;
            }
            let Some((canonical, _)) = conclusive_target(profile, index, call) else {
                continue;
            };
            if canonical != ANIMATION_GROUP && canonical != SUCCESSION {
                continue;
            }
            let class_name = canonical.rsplit('.').next().unwrap_or(canonical.as_str());
            let file = context.sources().file(call.file);
            let mut evidence = BTreeMap::new();
            evidence.insert("resolved".to_owned(), Value::String(canonical.clone()));
            evidence.insert("candidates".to_owned(), candidates_value(call));
            diagnostics.push(build_diagnostic(
                &MLC109,
                context,
                file,
                call.call_range,
                format!(
                    "Pass at least one animation to `{class_name}(...)`: keyword \
                     arguments such as `lag_ratio` configure timing but provide no \
                     animations, and an empty group fails at render time."
                ),
                format!(
                    "`{class_name}` derives its run time and interpolation plan from \
                     its member animations; with zero members the group cannot be \
                     compiled into a playable animation."
                ),
                evidence,
            ));
        }
        diagnostics
    }
}

// ---------------------------------------------------------------------------
// MLC122: ApplyMethod given a call result instead of the bound method.
// ---------------------------------------------------------------------------

/// Metadata for [`ApplyMethodCallResult`].
pub const MLC122: RuleMetadata = RuleMetadata {
    id: "MLC122",
    summary: "ApplyMethod receives a method call result, not the bound method",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::High,
    implementation_phase: 1,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

/// `ApplyMethod(mob.shift(...))` instead of `ApplyMethod(mob.shift, ...)`
/// (DESIGN §7.1 `MLC122`).
pub struct ApplyMethodCallResult;

impl Rule for ApplyMethodCallResult {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLC122
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let calls = context.qualified_calls();
        let mut diagnostics = Vec::new();
        for call in &calls.calls {
            let Some((canonical, _)) = conclusive_target(profile, index, call) else {
                continue;
            };
            if canonical != APPLY_METHOD {
                continue;
            }
            let Some(argument) = call.positional(0) else {
                continue;
            };
            let ArgShape::Call(inner_index) = &argument.shape else {
                continue;
            };
            let inner = &calls.calls[*inner_index];
            let file = context.sources().file(call.file);
            let Some(InnerVerdict::MethodResult(resolved_method)) =
                inner_verdict(profile, index, file, inner)
            else {
                continue;
            };
            let callee_text = file.slice(inner.callee_range);
            let mut evidence = BTreeMap::new();
            evidence.insert(
                "resolved".to_owned(),
                Value::String(APPLY_METHOD.to_owned()),
            );
            evidence.insert(
                "resolved_argument".to_owned(),
                Value::String(resolved_method),
            );
            let mut diagnostic = build_diagnostic(
                &MLC122,
                context,
                file,
                argument.range,
                format!(
                    "Pass the bound method and its arguments separately: \
                     `ApplyMethod({callee_text}, ...)`. `{callee_text}(...)` runs the \
                     mutation immediately and hands `ApplyMethod` a mobject, not a \
                     method."
                ),
                "`ApplyMethod(method, *args)` expects an uncalled bound mobject method; \
                 it calls the method itself during interpolation. Passing the call \
                 *result* mutates the mobject before the play starts and raises at \
                 animation build time because a mobject is not callable the way \
                 `ApplyMethod` requires."
                    .to_owned(),
                evidence,
            );
            diagnostic.fix = unbundle_fix(file, argument.range, inner, callee_text);
            diagnostics.push(diagnostic);
        }
        diagnostics
    }
}

/// Unsafe suggestion rewriting `ApplyMethod(mob.shift(RIGHT))` to
/// `ApplyMethod(mob.shift, RIGHT)`.
fn unbundle_fix(
    file: &SourceFile,
    argument_range: rustpython_parser::text_size::TextRange,
    inner: &QualifiedCall,
    callee_text: &str,
) -> Option<Fix> {
    if !simple_dotted(callee_text)
        || inner.has_star_args
        || inner.has_star_star_kwargs
        || inner
            .arguments
            .iter()
            .any(|argument| argument.keyword.is_some())
    {
        return None;
    }
    let mut parts = vec![callee_text.to_owned()];
    for argument in &inner.arguments {
        parts.push(file.slice(argument.range).to_owned());
    }
    let replacement = parts.join(", ");
    Some(Fix {
        applicability: FixApplicability::Unsafe,
        message: format!("Split `{callee_text}(...)` into `{replacement}`"),
        edits: vec![TextEdit {
            path: file.relative_path().to_owned(),
            span: file.span_of_range(argument_range),
            replacement,
        }],
    })
}
