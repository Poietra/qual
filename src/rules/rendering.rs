//! Rendering / geometry / renderer-compatibility rules (`MLR1xx`, DESIGN §7.2).
//!
//! Phase 1 implements the direct-call / literal rules `MLR101`, `MLR103`,
//! `MLR104`, `MLR105`, `MLR106`, `MLR115`, `MLR117`, `MLR124`, and `MLR126`.
//! Every rule fires only on confirmed facts: qualified-call candidates that
//! resolve through the loaded knowledge profile, literal argument facts, and
//! straight-line instance kinds. Unknown facts always mean silence
//! (DESIGN §15 invariant 2).
//!
//! Phase 2 adds the state-dependent rules `MLR102`, `MLR113`, and `MLR125`
//! over lifecycle facts (`rendering/state.rs`), plus the literal-driven
//! `MLR114` (`rendering/points.rs`) and `MLR127` (`rendering/tex_keys.rs`).
//!
//! The renderer-compatibility wave adds `MLR116` over the interpreter's
//! own-path state (`rendering/path_state.rs`), the renderer-profile-gated
//! rules `MLR107` / `MLR119` / `MLR120` (`rendering/renderer.rs`),
//! `MLR108` (`rendering/fixed_visibility.rs`), `MLR111`
//! (`rendering/scene_updaters.rs`), `MLR112`
//! (`rendering/points_layout.rs`), `MLR121` (`rendering/stacking.rs`),
//! and the conservative literal-TeX balance rule `MLR110`
//! (`rendering/tex_balance.rs`). Renderer-specific diagnostics restrict
//! `applicable_profiles` to the affected renderer's profiles and stay
//! silent when no active profile targets it (DESIGN §15 invariant 8).
//!
//! Still reserved: `MLR109` (updater ordering lag), `MLR118` (SVG asset
//! content facts), `MLR122` (the interpreter now tracks per-object
//! `z_index`, but the alias-safe cross-object stacking proof — "a literal
//! `set_z_index` on another object provably keeps this one below it" — is
//! still missing), and `MLR123` (no curated mesh / `Object3D` class in
//! the knowledge profile).

mod assets;
mod fixed_visibility;
mod markup;
mod path_state;
mod points;
mod points_layout;
mod renderer;
mod scene_updaters;
mod stacking;
mod state;
mod tex;
mod tex_balance;
mod tex_keys;

/// Shared with `rules::portability::paths` (MLD303/MLD305): recognizing
/// Windows drive prefixes and resolving a literal path against the
/// filesystem case-insensitively are one behavior each, owned here next
/// to MLR104.
pub(crate) use assets::{case_insensitive_match, has_drive_prefix};

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rustpython_parser::Tok;
use rustpython_parser::text_size::TextRange;
use serde_json::{Value, json};

use crate::config::model::Platform;
use crate::diagnostic::{
    Confidence, Diagnostic, Fix, FixApplicability, RuleMetadata, Severity, SourceSpan, TextEdit,
};
use crate::frontend::index::{
    ArgShape, CallArgument, LiteralFact, ProjectIndex, QualifiedCall, ReceiverKind,
};
use crate::frontend::statements::{FileBindingFacts, StatementRole};
use crate::knowledge::{AcceptedTarget, KnowledgeProfile, SymbolEntry, SymbolKind};
use crate::rules::base::{Rule, RuleContext};
use crate::source::SourceFile;

/// Every implemented rendering rule, in rule-ID order.
///
/// The registry composes this with the other rule-group modules; adding a
/// rule here is the only registration step a rendering rule needs.
#[must_use]
pub fn rules() -> Vec<Box<dyn Rule>> {
    vec![
        Box::new(NonVmobjectCreationTarget),
        Box::new(state::BarePlayedAnimate),
        Box::new(TexEscapeCollision),
        Box::new(UnresolvedAssetPath),
        Box::new(InvalidMarkupLiteral),
        Box::new(NonFiniteGeometryLiteral),
        Box::new(renderer::UnsupportedRendererApi),
        Box::new(fixed_visibility::StaleFixedVisibility),
        Box::new(tex_balance::UnbalancedTexLiteral),
        Box::new(scene_updaters::SceneUpdaterMutatesMobject),
        Box::new(points_layout::FixedPointLayoutAssumption),
        Box::new(state::SelfTransform),
        Box::new(points::NonPoint3Literal),
        Box::new(NonPositiveFontSize),
        Box::new(path_state::EmptyPathEdit),
        Box::new(BareRegisterFont),
        Box::new(renderer::MovingCameraUnderOpengl),
        Box::new(renderer::FocalDistanceUnderOpengl),
        Box::new(stacking::ZShiftForStacking),
        Box::new(MarkupInPlainText),
        Box::new(state::BareMobjectLeaf),
        Box::new(OpacityStrokeRange),
        Box::new(tex_keys::UnmatchableTexKey),
    ]
}

// ---------------------------------------------------------------------------
// Canonical symbol ids used by these rules.
// ---------------------------------------------------------------------------

const MATH_TEX: &str = "manim.mobject.text.tex_mobject.MathTex";
const TEX: &str = "manim.mobject.text.tex_mobject.Tex";
const SINGLE_MATH_TEX: &str = "manim.mobject.text.tex_mobject.SingleStringMathTex";
const TEXT: &str = "manim.mobject.text.text_mobject.Text";
const MARKUP_TEXT: &str = "manim.mobject.text.text_mobject.MarkupText";
const SVG_MOBJECT: &str = "manim.mobject.svg.svg_mobject.SVGMobject";
const IMAGE_MOBJECT: &str = "manim.mobject.types.image_mobject.ImageMobject";
const REGISTER_FONT: &str = "manim.mobject.text.text_mobject.register_font";

/// Curated knowledge methods whose arguments are geometry (positions,
/// factors, angles, buffers) for `MLR106`.
const GEOMETRY_METHODS: &[&str] = &["move_to", "next_to", "rotate", "scale", "shift", "to_edge"];

/// Constructors whose `font_size` keyword `MLR115` validates.
const FONT_SIZE_CONSTRUCTORS: &[&str] = &[MATH_TEX, MARKUP_TEXT, SINGLE_MATH_TEX, TEX, TEXT];

/// Constructors whose positional TeX strings `MLR103` inspects.
const TEX_CONSTRUCTORS: &[&str] = &[MATH_TEX, SINGLE_MATH_TEX, TEX];

// ---------------------------------------------------------------------------
// Shared fact helpers.
// ---------------------------------------------------------------------------

/// The parser byte range of a lifecycle allocation site.
pub(crate) fn site_range(site: crate::semantic::values::AllocationSite) -> TextRange {
    TextRange::new(site.start.into(), site.end.into())
}

/// The qualified-call fact whose whole call expression sits exactly at
/// `site`, if the frontend collected one.
pub(crate) fn call_at(
    facts: &crate::frontend::index::QualifiedCallFacts,
    site: crate::semantic::values::AllocationSite,
) -> Option<&QualifiedCall> {
    facts.calls.iter().find(|call| {
        call.file == site.file
            && u32::from(call.call_range.start()) == site.start
            && u32::from(call.call_range.end()) == site.end
    })
}

/// Names of the active profiles targeting `renderer`, in run order.
///
/// Renderer-specific rules restrict `applicable_profiles` to this set and
/// stay silent when it is empty (DESIGN §15 invariant 8).
pub(crate) fn renderer_profile_names(
    context: &RuleContext<'_>,
    renderer: crate::config::model::Renderer,
) -> Vec<String> {
    context
        .active_profiles()
        .iter()
        .filter(|profile| profile.renderer == renderer)
        .map(|profile| profile.name.clone())
        .collect()
}

/// The single knowledge entry every candidate of the set resolves to.
///
/// `None` when the set is empty, ambiguous, or contains an id the profile
/// does not curate — resolution never guesses.
pub(crate) fn single_knowledge_symbol<'a>(
    profile: &'a KnowledgeProfile,
    candidates: &BTreeSet<String>,
) -> Option<(&'a str, &'a SymbolEntry)> {
    if candidates.len() != 1 {
        return None;
    }
    let id = candidates.iter().next()?;
    let (key, entry) = profile.symbols.get_key_value(id)?;
    Some((key.as_str(), entry))
}

/// Resolves one method candidate (`<class id>.<method>`) to its canonical
/// curated method entry by walking the knowledge base-class chain, mirroring
/// the Python MRO over curated bases only.
fn resolve_method_symbol<'a>(
    profile: &'a KnowledgeProfile,
    candidate: &str,
) -> Option<(String, &'a SymbolEntry)> {
    if let Some(entry) = profile.symbol(candidate) {
        return (entry.kind == SymbolKind::Method).then(|| (candidate.to_owned(), entry));
    }
    let (class_id, method) = candidate.rsplit_once('.')?;
    let mut queue = vec![class_id.to_owned()];
    let mut visited = BTreeSet::new();
    let mut cursor = 0;
    while cursor < queue.len() {
        let class = queue[cursor].clone();
        cursor += 1;
        if !visited.insert(class.clone()) {
            continue;
        }
        let qualified = format!("{class}.{method}");
        if let Some(entry) = profile.symbol(&qualified) {
            if entry.kind == SymbolKind::Method {
                return Some((qualified, entry));
            }
        }
        if let Some(class_entry) = profile.symbol(&class) {
            queue.extend(class_entry.bases.iter().cloned());
        }
    }
    None
}

/// Resolves a whole call's candidate set to one canonical curated method.
///
/// Every candidate must resolve to the same canonical id; anything else
/// (empty set, unresolved candidate, ambiguity) yields `None`.
fn resolved_method_for_call<'a>(
    profile: &'a KnowledgeProfile,
    call: &QualifiedCall,
) -> Option<(String, &'a SymbolEntry)> {
    let mut resolved: Option<(String, &SymbolEntry)> = None;
    if call.candidates.is_empty() {
        return None;
    }
    for candidate in &call.candidates {
        let (id, entry) = resolve_method_symbol(profile, candidate)?;
        match &resolved {
            None => resolved = Some((id, entry)),
            Some((existing, _)) if *existing == id => {}
            Some(_) => return None,
        }
    }
    resolved
}

/// Vectorization classification of a class id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VmClass {
    /// A `VMobject` (vectorized) class.
    Vectorized,
    /// A confirmed plain (non-vectorized) `Mobject` class.
    Plain,
    /// Not enough facts to classify.
    Unknown,
}

/// Classifies a class id via the knowledge profile, falling back to the
/// project class hierarchy (whose `reached_bases` are canonical ids).
///
/// `Plain` is only returned when the whole base chain is resolved and every
/// reached base is a curated non-vectorized kind; a single unknown base
/// keeps the verdict `Unknown` (silence over false positives).
fn classify_class(profile: &KnowledgeProfile, index: &ProjectIndex, id: &str) -> VmClass {
    if let Some(entry) = profile.symbol(id) {
        return match entry.kind {
            SymbolKind::Vmobject => VmClass::Vectorized,
            SymbolKind::Mobject => VmClass::Plain,
            _ => VmClass::Unknown,
        };
    }
    let Some(class) = index.classes.get(id) else {
        return VmClass::Unknown;
    };
    let mut plain = false;
    let mut unknown = false;
    for base in &class.reached_bases {
        match classify_class(profile, index, base) {
            VmClass::Vectorized => return VmClass::Vectorized,
            VmClass::Plain => plain = true,
            VmClass::Unknown => unknown = true,
        }
    }
    if plain && !unknown && class.bases_fully_resolved {
        VmClass::Plain
    } else {
        VmClass::Unknown
    }
}

/// Classifies a whole candidate set: all ids must agree on a verdict.
fn classify_candidates(
    profile: &KnowledgeProfile,
    index: &ProjectIndex,
    candidates: &BTreeSet<String>,
) -> VmClass {
    let mut verdict: Option<VmClass> = None;
    if candidates.is_empty() {
        return VmClass::Unknown;
    }
    for id in candidates {
        let class = classify_class(profile, index, id);
        match (verdict, class) {
            (_, VmClass::Unknown) => return VmClass::Unknown,
            (None, current) => verdict = Some(current),
            (Some(previous), current) if previous == current => {}
            (Some(_), _) => return VmClass::Unknown,
        }
    }
    verdict.unwrap_or(VmClass::Unknown)
}

/// Whether the argument entry at `index` is a `**kwargs` splat (keyword
/// `None` past the positional slots).
fn is_kwargs_splat(call: &QualifiedCall, index: usize, argument: &CallArgument) -> bool {
    argument.keyword.is_none() && index >= call.positional_count
}

/// The literal numeric value of an argument, when statically known.
fn numeric_literal(argument: &CallArgument) -> Option<f64> {
    match argument.literal.as_ref()? {
        #[allow(clippy::cast_precision_loss, reason = "range check, not arithmetic")]
        LiteralFact::Int(value) => Some(*value as f64),
        LiteralFact::Float(value) => Some(*value),
        _ => None,
    }
}

/// The short display name of a canonical id (`Create` from
/// `manim.animation.creation.Create`).
pub(crate) fn short_name(id: &str) -> &str {
    id.rsplit('.').next().unwrap_or(id)
}

/// Builds a diagnostic with the shared per-rule boilerplate filled in.
#[allow(
    clippy::too_many_arguments,
    reason = "one flat builder keeps rules terse"
)]
pub(crate) fn build_diagnostic(
    metadata: &RuleMetadata,
    file: &SourceFile,
    range: TextRange,
    confidence: Confidence,
    message: String,
    explanation: &str,
    evidence: BTreeMap<String, Value>,
    applicable_profiles: Vec<String>,
    fix: Option<Fix>,
) -> Diagnostic {
    Diagnostic {
        rule_id: metadata.id.to_owned(),
        severity: metadata.default_severity,
        confidence,
        path: file.relative_path().to_owned(),
        primary_span: file.span_of_range(range),
        message,
        explanation: Some(explanation.to_owned()),
        related_locations: Vec::new(),
        evidence,
        estimated_cost: None,
        applicable_profiles,
        fix,
    }
}

// ---------------------------------------------------------------------------
// Source / token helpers for fixes.
// ---------------------------------------------------------------------------

/// Whether `range` covers exactly one string token (no implicit
/// concatenation), so a textual edit maps 1:1 onto the literal.
fn is_single_string_token(file: &SourceFile, range: TextRange) -> bool {
    let mut count = 0;
    for (token, token_range) in file.tokens() {
        if token_range.start() < range.start() {
            continue;
        }
        if token_range.start() >= range.end() {
            break;
        }
        if matches!(token, Tok::String { .. }) {
            count += 1;
        }
    }
    count == 1
}

/// Zero-width span at a byte offset, for insertion edits.
fn insertion_span(file: &SourceFile, byte: usize) -> SourceSpan {
    let position = file.position_of_byte(byte);
    SourceSpan {
        start: position,
        end: position,
    }
}

/// Length in bytes of the literal's closing quote run (1 or 3), when the
/// slice ends in a quote character.
fn closing_quote_len(slice: &str) -> Option<usize> {
    let last = slice.chars().last()?;
    if last != '"' && last != '\'' {
        return None;
    }
    let triple: String = std::iter::repeat_n(last, 3).collect();
    Some(if slice.len() >= 3 && slice.ends_with(&triple) {
        3
    } else {
        1
    })
}

// ---------------------------------------------------------------------------
// MLR106 name facts over the frontend binding layer.
// ---------------------------------------------------------------------------

/// Classifies a bare name as a trusted `math.inf` / `math.nan` alias via
/// the frontend's conservative per-file binding facts (an import binding
/// survives there only when nothing else in the file rebinds the name).
/// Returns the display kind (`"infinity"` / `"NaN"`).
fn nonfinite_math_alias(bindings: &FileBindingFacts, name: &str) -> Option<&'static str> {
    match bindings.import_target(name)? {
        "math.inf" => Some("infinity"),
        "math.nan" => Some("NaN"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// MLR101: Create/Uncreate/Write/Unwrite/DrawBorderThenFill on a confirmed
// non-VMobject target.
// ---------------------------------------------------------------------------

const MLR101: RuleMetadata = RuleMetadata {
    id: "MLR101",
    summary: "Create/Write-style animations require a vectorized (VMobject) target",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::High,
    implementation_phase: 1,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

struct NonVmobjectCreationTarget;

impl Rule for NonVmobjectCreationTarget {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR101
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let index = context.project_index();
        let facts = context.qualified_calls();
        let profiles = context.config().active_profile_names();
        let mut diagnostics = Vec::new();
        for call in &facts.calls {
            let Some((animation_id, entry)) = single_knowledge_symbol(profile, &call.candidates)
            else {
                continue;
            };
            if entry.kind != SymbolKind::Animation
                || entry.accepted_target != Some(AcceptedTarget::Vmobject)
            {
                continue;
            }
            let Some(argument) = call.positional(0) else {
                continue;
            };
            let animation = short_name(animation_id);
            let (described, kinds): (String, Vec<String>) = if argument.literal.is_some() {
                let file = context.sources().file(call.file);
                (
                    format!("the literal `{}`", file.slice(argument.range)),
                    Vec::new(),
                )
            } else {
                let candidates = match &argument.shape {
                    ArgShape::Call(inner) => &facts.calls[*inner].candidates,
                    ArgShape::Name => &argument.kind_candidates,
                    _ => continue,
                };
                if classify_candidates(profile, index, candidates) != VmClass::Plain {
                    continue;
                }
                let names: Vec<String> = candidates
                    .iter()
                    .map(|id| short_name(id).to_owned())
                    .collect();
                (
                    format!("a plain (non-vectorized) `{}`", names.join("` / `")),
                    candidates.iter().cloned().collect(),
                )
            };
            let file = context.sources().file(call.file);
            let mut evidence = BTreeMap::new();
            evidence.insert("animation".to_owned(), json!(animation_id));
            evidence.insert("target_kinds".to_owned(), json!(kinds));
            diagnostics.push(build_diagnostic(
                &MLR101,
                file,
                argument.range,
                Confidence::High,
                format!(
                    "`{animation}()` requires a vectorized mobject (VMobject); this target is {described}"
                ),
                "Create-style animations draw the Bézier outline of a VMobject. \
                 A plain Mobject (e.g. ImageMobject or a bare Mobject) has no such \
                 curves, so Manim raises a TypeError when the animation begins.",
                evidence,
                profiles.clone(),
                None,
            ));
        }
        diagnostics
    }
}

// ---------------------------------------------------------------------------
// MLR103: Python escape / TeX command collision in non-raw Tex literals.
// ---------------------------------------------------------------------------

const MLR103: RuleMetadata = RuleMetadata {
    id: "MLR103",
    summary: "Python escape in a non-raw Tex/MathTex literal corrupts a TeX command",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::High,
    implementation_phase: 1,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

struct TexEscapeCollision;

impl Rule for TexEscapeCollision {
    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let profiles = context.config().active_profile_names();
        let mut diagnostics = Vec::new();
        for call in &context.qualified_calls().calls {
            let Some((constructor_id, _)) = single_knowledge_symbol(profile, &call.candidates)
            else {
                continue;
            };
            if !TEX_CONSTRUCTORS.contains(&constructor_id) {
                continue;
            }
            let file = context.sources().file(call.file);
            for argument in call
                .arguments
                .iter()
                .filter(|argument| argument.keyword.is_none())
            {
                let Some(LiteralFact::Str { prefix, range, .. }) = &argument.literal else {
                    continue;
                };
                if prefix.raw || prefix.bytes || prefix.formatted {
                    continue;
                }
                let raw_text = file.slice(*range);
                let collisions = tex::find_collisions(raw_text);
                let Some(first) = collisions.first() else {
                    continue;
                };
                let constructor = short_name(constructor_id);
                let commands: Vec<&str> = collisions
                    .iter()
                    .map(|collision| collision.command.as_str())
                    .collect();
                let more = if collisions.len() > 1 {
                    format!(
                        " ({} colliding escapes in this literal: \\{})",
                        collisions.len(),
                        commands.join(", \\")
                    )
                } else {
                    String::new()
                };
                let mut evidence = BTreeMap::new();
                evidence.insert("constructor".to_owned(), json!(constructor_id));
                evidence.insert("commands".to_owned(), json!(commands));
                diagnostics.push(build_diagnostic(
                    &MLR103,
                    file,
                    *range,
                    Confidence::High,
                    format!(
                        "Non-raw string passed to `{constructor}()`: Python reads \
                         `{escape}` in `\\{command}` as a {control} control character, \
                         so TeX never sees `\\{command}`{more}",
                        escape = first.escape,
                        command = first.command,
                        control = first.control_character,
                    ),
                    "In a non-raw Python string literal the backslash starts a Python \
                     escape sequence, not a TeX command. The compiled TeX source \
                     contains a control character instead of the command, so the \
                     compile fails or renders wrong output. Use a raw string \
                     (r\"...\") or escape the backslash (\"\\\\...\").",
                    evidence,
                    profiles.clone(),
                    raw_prefix_fix(file, *range),
                ));
            }
        }
        diagnostics
    }

    fn metadata(&self) -> &'static RuleMetadata {
        &MLR103
    }
}

/// UNSAFE fix: insert an `r` prefix before a single plain string token.
/// A raw prefix changes the runtime value of every escape in the literal,
/// so this is never applied without `--unsafe-fixes`.
fn raw_prefix_fix(file: &SourceFile, range: TextRange) -> Option<Fix> {
    if !is_single_string_token(file, range) {
        return None;
    }
    let slice = file.slice(range);
    if !slice.starts_with('"') && !slice.starts_with('\'') {
        return None;
    }
    Some(Fix {
        applicability: FixApplicability::Unsafe,
        message: "Add an `r` prefix. Unsafe: a raw prefix changes the runtime value \
                  of every escape in this literal (each `\\x` becomes a literal \
                  backslash followed by `x`)."
            .to_owned(),
        edits: vec![TextEdit {
            path: file.relative_path().to_owned(),
            span: insertion_span(file, usize::from(range.start())),
            replacement: "r".to_owned(),
        }],
    })
}

// ---------------------------------------------------------------------------
// MLR104: literal asset path that Manim's runtime search cannot resolve.
// ---------------------------------------------------------------------------

const MLR104: RuleMetadata = RuleMetadata {
    id: "MLR104",
    summary: "Literal asset path does not resolve in the render search path",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::High,
    implementation_phase: 1,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

struct UnresolvedAssetPath;

impl Rule for UnresolvedAssetPath {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR104
    }

    #[allow(
        clippy::too_many_lines,
        reason = "profile grouping and two diagnostic shapes"
    )]
    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let project_root = context.sources().project_root();
        let mut diagnostics = Vec::new();
        for call in &context.qualified_calls().calls {
            let Some((constructor_id, _)) = single_knowledge_symbol(profile, &call.candidates)
            else {
                continue;
            };
            let (extensions, parameter) = match constructor_id {
                SVG_MOBJECT => (assets::SVG_EXTENSIONS, "file_name"),
                IMAGE_MOBJECT => (assets::RASTER_EXTENSIONS, "filename_or_array"),
                _ => continue,
            };
            let Some(argument) = call.keyword(parameter).or_else(|| call.positional(0)) else {
                continue;
            };
            let Some(LiteralFact::Str {
                value,
                prefix,
                range,
            }) = &argument.literal
            else {
                continue;
            };
            if prefix.bytes || value.is_empty() {
                continue;
            }
            let file = context.sources().file(call.file);

            let mut case_matches: BTreeMap<String, Vec<(String, Platform)>> = BTreeMap::new();
            let mut missing_profiles: Vec<String> = Vec::new();
            let mut missing_platforms: Vec<Platform> = Vec::new();
            let mut tried: Vec<String> = Vec::new();
            for render_profile in context.active_profiles() {
                let working_dir = project_root.join(&render_profile.working_directory);
                match assets::resolve_literal(
                    project_root,
                    &working_dir,
                    &render_profile.assets_dir,
                    value,
                    extensions,
                ) {
                    assets::AssetResolution::Found | assets::AssetResolution::Unverifiable => {}
                    assets::AssetResolution::CaseOnlyMismatch { corrected } => {
                        case_matches
                            .entry(corrected)
                            .or_default()
                            .push((render_profile.name.clone(), render_profile.platform));
                    }
                    assets::AssetResolution::NotFound { tried: candidates } => {
                        missing_profiles.push(render_profile.name.clone());
                        missing_platforms.push(render_profile.platform);
                        if tried.is_empty() {
                            tried = candidates;
                        }
                    }
                }
            }
            let constructor = short_name(constructor_id);
            for (corrected, profiles) in case_matches {
                // DESIGN §7.2 prose: the case-only mismatch is presented
                // per target platform. The lookup fails only on
                // case-sensitive targets (linux); on a windows/macos
                // target the render's case-insensitive filesystem lookup
                // finds the file and the render succeeds, so those
                // profiles are not applicable — and when NO case-sensitive
                // target is affected the error would claim a render
                // failure that does not happen on any declared target
                // (AGENTS rule 4). DESIGN §6 fixes one severity per rule
                // ID, so the rule stays silent rather than downgrading;
                // a project that only declares case-insensitive targets
                // has declared that this render succeeds as written.
                let applicable: Vec<String> = profiles
                    .iter()
                    .filter(|(_, platform)| *platform == Platform::Linux)
                    .map(|(name, _)| name.clone())
                    .collect();
                if applicable.is_empty() {
                    continue;
                }
                let wording = if applicable.len() == profiles.len() {
                    "the lookup is case-sensitive on linux"
                } else {
                    "case-insensitive filesystems may mask this locally, but the \
                     path as written does not match the on-disk case"
                };
                let mut evidence = BTreeMap::new();
                evidence.insert("constructor".to_owned(), json!(constructor_id));
                evidence.insert("literal".to_owned(), json!(value));
                evidence.insert("corrected".to_owned(), json!(corrected));
                diagnostics.push(build_diagnostic(
                    &MLR104,
                    file,
                    *range,
                    Confidence::High,
                    format!(
                        "Asset path \"{value}\" passed to `{constructor}()` only matches \
                         \"{corrected}\" case-insensitively; {wording}"
                    ),
                    "Manim resolves the file name against the render working directory \
                     and then assets_dir; the lookup uses the exact character case of \
                     the path, so a case-only mismatch fails on case-sensitive \
                     filesystems.",
                    evidence,
                    applicable,
                    case_correction_fix(file, *range, &corrected),
                ));
            }
            if !missing_profiles.is_empty() {
                let source_dir = file.path().parent().map(std::path::Path::to_path_buf);
                let next_to_source = source_dir
                    .as_deref()
                    .is_some_and(|dir| assets::exists_relative_to(dir, value));
                let aside = if next_to_source {
                    " (a matching file exists next to the source file, but Manim \
                     does not search there)"
                } else {
                    ""
                };
                let mut evidence = BTreeMap::new();
                evidence.insert("constructor".to_owned(), json!(constructor_id));
                evidence.insert("literal".to_owned(), json!(value));
                evidence.insert("tried".to_owned(), json!(tried));
                evidence.insert("exists_next_to_source".to_owned(), json!(next_to_source));
                // DESIGN §7.2 prose: validity is decided by performing the
                // same search Manim's runtime would, on this machine. For
                // an absolute path outside the project tree that check is
                // inherently about the lint host, not necessarily the
                // render host (CI linting a repo rendered elsewhere), so
                // the environment dependence is declared as evidence. The
                // metadata floor (§6: confidence at or above the rule
                // minimum) keeps this at high rather than capping lower.
                if Path::new(value.as_str()).is_absolute()
                    && !Path::new(value.as_str()).starts_with(project_root)
                {
                    evidence.insert("environment_dependent".to_owned(), json!(true));
                }
                diagnostics.push(build_diagnostic(
                    &MLR104,
                    file,
                    *range,
                    Confidence::High,
                    format!(
                        "Asset \"{value}\" passed to `{constructor}()` does not resolve \
                         from the render working directory or assets_dir; Manim raises \
                         OSError before rendering{aside}"
                    ),
                    "Manim resolves file_name as Path(file_name) against the render \
                     working directory, then as assets_dir/file_name with the known \
                     extensions appended. The source file's directory and the project \
                     root are not part of that search.",
                    evidence,
                    missing_profiles,
                    None,
                ));
            }
        }
        diagnostics
    }
}

/// SAFE fix: rewrite the literal with the on-disk character case. The
/// runtime string changes only in case, matching the file that actually
/// exists, so behavior is preserved (the previous path did not resolve).
fn case_correction_fix(file: &SourceFile, range: TextRange, corrected: &str) -> Option<Fix> {
    if !is_single_string_token(file, range) {
        return None;
    }
    let slice = file.slice(range);
    let quote_position = slice.find(['"', '\''])?;
    let quote = slice[quote_position..].chars().next()?;
    let triple: String = std::iter::repeat_n(quote, 3).collect();
    let quote_len = if slice[quote_position..].starts_with(&triple) {
        3
    } else {
        1
    };
    if slice.len() < quote_position + 2 * quote_len {
        return None;
    }
    let opening = &slice[..quote_position + quote_len];
    let closing = &slice[slice.len() - quote_len..];
    Some(Fix {
        applicability: FixApplicability::Safe,
        message: format!("Use the on-disk case: \"{corrected}\""),
        edits: vec![TextEdit {
            path: file.relative_path().to_owned(),
            span: file.span_of_range(range),
            replacement: format!("{opening}{corrected}{closing}"),
        }],
    })
}

// ---------------------------------------------------------------------------
// MLR105: provable Pango markup errors in MarkupText literals.
// ---------------------------------------------------------------------------

const MLR105: RuleMetadata = RuleMetadata {
    id: "MLR105",
    summary: "MarkupText literal contains provably invalid Pango markup",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::High,
    implementation_phase: 1,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

struct InvalidMarkupLiteral;

impl Rule for InvalidMarkupLiteral {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR105
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let profiles = context.config().active_profile_names();
        let mut diagnostics = Vec::new();
        for call in &context.qualified_calls().calls {
            let Some((constructor_id, _)) = single_knowledge_symbol(profile, &call.candidates)
            else {
                continue;
            };
            if constructor_id != MARKUP_TEXT {
                continue;
            }
            let Some(argument) = call.keyword("text").or_else(|| call.positional(0)) else {
                continue;
            };
            let Some(LiteralFact::Str {
                value,
                prefix,
                range,
            }) = &argument.literal
            else {
                continue;
            };
            if prefix.bytes {
                continue;
            }
            let markup::MarkupVerdict::Invalid(error) = markup::validate(value) else {
                continue;
            };
            let file = context.sources().file(call.file);
            let fix = if let markup::MarkupError::Unclosed { open } = &error {
                append_closing_tag_fix(file, *range, open)
            } else {
                None
            };
            let mut evidence = BTreeMap::new();
            evidence.insert("constructor".to_owned(), json!(MARKUP_TEXT));
            evidence.insert("error".to_owned(), json!(error.message()));
            diagnostics.push(build_diagnostic(
                &MLR105,
                file,
                *range,
                Confidence::High,
                format!(
                    "`MarkupText()` literal is invalid Pango markup: {}",
                    error.message()
                ),
                "Pango parses the markup when the text is laid out, so MarkupText \
                 raises a rendering error for mismatched or unclosed tags and \
                 unknown entities. Only the validated Pango subset is judged; \
                 anything outside it is left alone.",
                evidence,
                profiles.clone(),
                fix,
            ));
        }
        diagnostics
    }
}

/// UNSAFE fix: append the missing closing tag just before the closing
/// quote. Unsafe because the intended close position is unknowable.
fn append_closing_tag_fix(file: &SourceFile, range: TextRange, tag: &str) -> Option<Fix> {
    if !is_single_string_token(file, range) {
        return None;
    }
    let slice = file.slice(range);
    let quote_len = closing_quote_len(slice)?;
    Some(Fix {
        applicability: FixApplicability::Unsafe,
        message: format!(
            "Append `</{tag}>` at the end of the literal. Unsafe: the intended \
             close position may be earlier."
        ),
        edits: vec![TextEdit {
            path: file.relative_path().to_owned(),
            span: insertion_span(file, usize::from(range.end()) - quote_len),
            replacement: format!("</{tag}>"),
        }],
    })
}

// ---------------------------------------------------------------------------
// MLR106: literal NaN / inf flowing into mobject geometry.
// ---------------------------------------------------------------------------

const MLR106: RuleMetadata = RuleMetadata {
    id: "MLR106",
    summary: "Literal NaN/inf flows into mobject geometry",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::High,
    implementation_phase: 1,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

struct NonFiniteGeometryTarget {
    display: String,
    canonical: String,
}

struct NonFiniteGeometryLiteral;

impl Rule for NonFiniteGeometryLiteral {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR106
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let facts = context.qualified_calls();
        let profiles = context.config().active_profile_names();
        let mut diagnostics = Vec::new();
        for call in &facts.calls {
            let Some(target) = nonfinite_rule_target(profile, call) else {
                continue;
            };
            let file = context.sources().file(call.file);
            let Some(bindings) = context.binding_facts().file(call.file) else {
                continue;
            };
            for (position, argument) in call.arguments.iter().enumerate() {
                if is_kwargs_splat(call, position, argument) {
                    continue;
                }
                let Some(kind) = nonfinite_argument(file, facts, bindings, argument) else {
                    continue;
                };
                let written = file.slice(argument.range);
                let mut evidence = BTreeMap::new();
                evidence.insert("target".to_owned(), json!(target.canonical));
                evidence.insert("value".to_owned(), json!(kind));
                evidence.insert("expression".to_owned(), json!(written));
                diagnostics.push(build_diagnostic(
                    &MLR106,
                    file,
                    argument.range,
                    Confidence::High,
                    format!(
                        "`{written}` is {kind}; passing it to `{display}` puts a \
                         non-finite value into the mobject's geometry",
                        display = target.display,
                    ),
                    "NaN and infinity propagate through the points array: bounding \
                     boxes, alignment, and camera transforms all become non-finite, \
                     so the mobject disappears or the renderer crashes with no \
                     obvious source location.",
                    evidence,
                    profiles.clone(),
                    None,
                ));
            }
        }
        diagnostics
    }
}

/// The confirmed geometry sink of a call, if any: a curated mobject
/// constructor or a curated fluent geometry method on a known instance.
fn nonfinite_rule_target(
    profile: &KnowledgeProfile,
    call: &QualifiedCall,
) -> Option<NonFiniteGeometryTarget> {
    if let Some((id, entry)) = single_knowledge_symbol(profile, &call.candidates) {
        if matches!(entry.kind, SymbolKind::Mobject | SymbolKind::Vmobject) {
            return Some(NonFiniteGeometryTarget {
                display: format!("{}()", short_name(id)),
                canonical: id.to_owned(),
            });
        }
        // A directly curated method id falls through to method resolution.
    }
    if !matches!(call.receiver, ReceiverKind::KnownInstance(_)) {
        return None;
    }
    let (canonical, _) = resolved_method_for_call(profile, call)?;
    let method = short_name(&canonical);
    if !GEOMETRY_METHODS.contains(&method) {
        return None;
    }
    Some(NonFiniteGeometryTarget {
        display: format!(".{method}()"),
        canonical,
    })
}

/// Classifies one argument value as a confirmed non-finite literal.
///
/// Returns `"NaN"` or `"infinity"`, or `None` for anything unconfirmed.
fn nonfinite_argument(
    file: &SourceFile,
    facts: &crate::frontend::index::QualifiedCallFacts,
    bindings: &FileBindingFacts,
    argument: &CallArgument,
) -> Option<&'static str> {
    match &argument.shape {
        ArgShape::Call(inner) => nonfinite_float_call(file, &facts.calls[*inner], bindings),
        ArgShape::Attribute(reference) => {
            if reference.candidates.len() != 1 {
                return None;
            }
            match reference.candidates.iter().next()?.as_str() {
                "math.inf" => Some("infinity"),
                "math.nan" => Some("NaN"),
                _ => None,
            }
        }
        ArgShape::Name => nonfinite_math_alias(bindings, file.slice(argument.range)),
        _ => None,
    }
}

/// Whether an inner call is the builtin `float(...)` over a non-finite
/// string literal (`float("nan")`, `float("-inf")`, case-insensitive).
fn nonfinite_float_call(
    file: &SourceFile,
    inner: &QualifiedCall,
    bindings: &FileBindingFacts,
) -> Option<&'static str> {
    if !bindings.is_unshadowed_bare("float")
        || !inner.candidates.is_empty()
        || inner.receiver != ReceiverKind::Direct
        || file.slice(inner.callee_range) != "float"
        || inner.has_star_args
        || inner.positional_count != 1
        || !inner.keyword_names.is_empty()
        || inner.has_star_star_kwargs
    {
        return None;
    }
    let argument = inner.positional(0)?;
    let Some(LiteralFact::Str { value, prefix, .. }) = &argument.literal else {
        return None;
    };
    if prefix.bytes {
        return None;
    }
    nonfinite_float_literal(value)
}

/// Classifies the string accepted by builtin `float` as NaN / infinity.
fn nonfinite_float_literal(value: &str) -> Option<&'static str> {
    let trimmed = value.trim().to_ascii_lowercase();
    let unsigned = trimmed.strip_prefix(['+', '-']).unwrap_or(trimmed.as_str());
    match unsigned {
        "nan" => Some("NaN"),
        "inf" | "infinity" => Some("infinity"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// MLR115: literal font_size <= 0.
// ---------------------------------------------------------------------------

const MLR115: RuleMetadata = RuleMetadata {
    id: "MLR115",
    summary: "Literal font_size is zero or negative",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::Certain,
    implementation_phase: 1,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

struct NonPositiveFontSize;

impl Rule for NonPositiveFontSize {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR115
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let profiles = context.config().active_profile_names();
        let mut diagnostics = Vec::new();
        for call in &context.qualified_calls().calls {
            let Some((constructor_id, _)) = single_knowledge_symbol(profile, &call.candidates)
            else {
                continue;
            };
            if !FONT_SIZE_CONSTRUCTORS.contains(&constructor_id) {
                continue;
            }
            let Some(argument) = call.keyword("font_size") else {
                continue;
            };
            let Some(value) = numeric_literal(argument) else {
                continue;
            };
            if value > 0.0 {
                continue;
            }
            let file = context.sources().file(call.file);
            let constructor = short_name(constructor_id);
            let written = file.slice(argument.range);
            let mut evidence = BTreeMap::new();
            evidence.insert("constructor".to_owned(), json!(constructor_id));
            evidence.insert("font_size".to_owned(), json!(value));
            diagnostics.push(build_diagnostic(
                &MLR115,
                file,
                argument.range,
                Confidence::Certain,
                format!(
                    "`{constructor}(font_size={written})` is not positive; text sizing \
                     requires font_size > 0"
                ),
                "Text and TeX mobjects scale their glyphs by font_size; zero or \
                 negative sizes produce empty or inverted geometry and can raise \
                 during layout.",
                evidence,
                profiles.clone(),
                None,
            ));
        }
        diagnostics
    }
}

// ---------------------------------------------------------------------------
// MLR117: bare register_font(...) call (context manager never entered).
// ---------------------------------------------------------------------------

const MLR117: RuleMetadata = RuleMetadata {
    id: "MLR117",
    summary: "register_font() context manager is called without `with`",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::High,
    implementation_phase: 1,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

struct BareRegisterFont;

impl Rule for BareRegisterFont {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR117
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let profiles = context.config().active_profile_names();
        let mut diagnostics = Vec::new();
        for call in &context.qualified_calls().calls {
            if call.candidates.len() != 1
                || call.candidates.iter().next().map(String::as_str) != Some(REGISTER_FONT)
            {
                continue;
            }
            // The context manager is discarded only when the call is a whole
            // bare expression statement (not a `with` item, not assigned,
            // not passed on).
            let is_bare = context
                .statement_facts()
                .for_call(call)
                .is_some_and(|statement| statement.role == StatementRole::BareExpression);
            if !is_bare {
                continue;
            }
            let file = context.sources().file(call.file);
            let mut evidence = BTreeMap::new();
            evidence.insert("function".to_owned(), json!(REGISTER_FONT));
            diagnostics.push(build_diagnostic(
                &MLR117,
                file,
                call.call_range,
                Confidence::High,
                "`register_font(...)` called as a bare statement registers nothing; \
                 wrap it in a `with` block"
                    .to_owned(),
                "register_font is a @contextmanager generator: calling it only \
                 creates the context object. The font is registered on __enter__ \
                 and unregistered on __exit__, so a bare call has no effect and \
                 the font is never available to Text.",
                evidence,
                profiles.clone(),
                with_wrap_fix(file, call),
            ));
        }
        diagnostics
    }
}

/// UNSAFE fix: wrap the bare call in `with ...:` with a `pass` body. The
/// statements that need the font must be moved into the block by hand.
fn with_wrap_fix(file: &SourceFile, call: &QualifiedCall) -> Option<Fix> {
    let span = file.span_of_range(call.call_range);
    let line_text = file.line_text(span.start.line);
    let indent: String = line_text
        .chars()
        .take_while(|character| character.is_whitespace())
        .collect();
    // Only when the call starts the line's content, so the rewrite cannot
    // corrupt statements sharing the line.
    if indent.chars().count() + 1 != span.start.column {
        return None;
    }
    let call_text = file.slice(call.call_range);
    Some(Fix {
        applicability: FixApplicability::Unsafe,
        message: "Wrap the call in `with register_font(...):`. Unsafe: the \
                  statements that use the font must be moved inside the block."
            .to_owned(),
        edits: vec![TextEdit {
            path: file.relative_path().to_owned(),
            span,
            replacement: format!("with {call_text}:\n{indent}    pass"),
        }],
    })
}

// ---------------------------------------------------------------------------
// MLR124: Pango markup inside a plain Text literal.
// ---------------------------------------------------------------------------

const MLR124: RuleMetadata = RuleMetadata {
    id: "MLR124",
    summary: "Text() literal contains Pango markup that plain Text renders verbatim",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::High,
    implementation_phase: 1,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

struct MarkupInPlainText;

impl Rule for MarkupInPlainText {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR124
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let profiles = context.config().active_profile_names();
        let mut diagnostics = Vec::new();
        for call in &context.qualified_calls().calls {
            let Some((constructor_id, _)) = single_knowledge_symbol(profile, &call.candidates)
            else {
                continue;
            };
            if constructor_id != TEXT {
                continue;
            }
            let Some(argument) = call.keyword("text").or_else(|| call.positional(0)) else {
                continue;
            };
            let Some(LiteralFact::Str {
                value,
                prefix,
                range,
            }) = &argument.literal
            else {
                continue;
            };
            if prefix.bytes {
                continue;
            }
            let Some(tag) = markup::matched_subset_pair(value) else {
                continue;
            };
            let file = context.sources().file(call.file);
            let mut evidence = BTreeMap::new();
            evidence.insert("constructor".to_owned(), json!(TEXT));
            evidence.insert("tag".to_owned(), json!(tag));
            diagnostics.push(build_diagnostic(
                &MLR124,
                file,
                *range,
                Confidence::High,
                format!(
                    "`Text()` renders `<{tag}>...</{tag}>` literally on screen; use \
                     `MarkupText()` for Pango markup"
                ),
                "Plain Text passes its string to Pango without markup parsing, so \
                 the tags appear verbatim in the rendered frame. MarkupText parses \
                 the same Pango markup subset.",
                evidence,
                profiles.clone(),
                markup_text_fix(context, call),
            ));
        }
        diagnostics
    }
}

/// UNSAFE fix: switch the constructor to `MarkupText`. Unsafe because the
/// rendered output changes (that is the point) and the name `MarkupText`
/// must be importable at the call site.
fn markup_text_fix(context: &RuleContext<'_>, call: &QualifiedCall) -> Option<Fix> {
    let file = context.sources().file(call.file);
    let callee = file.slice(call.callee_range);
    if callee != "Text" && !callee.ends_with(".Text") {
        return None;
    }
    let replacement = format!("{}MarkupText", &callee[..callee.len() - "Text".len()]);
    Some(Fix {
        applicability: FixApplicability::Unsafe,
        message: "Use MarkupText so the tags are parsed as Pango markup. Unsafe: \
                  the rendered output changes and MarkupText must be imported."
            .to_owned(),
        edits: vec![TextEdit {
            path: file.relative_path().to_owned(),
            span: file.span_of_range(call.callee_range),
            replacement,
        }],
    })
}

// ---------------------------------------------------------------------------
// MLR126: literal opacity outside [0, 1] / negative stroke width.
// ---------------------------------------------------------------------------

const MLR126: RuleMetadata = RuleMetadata {
    id: "MLR126",
    summary: "Literal opacity outside [0, 1] or negative stroke width",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::High,
    implementation_phase: 1,
    required_profiles: &[],
    required_capabilities: &["qualified-calls"],
    supersedes: &[],
};

/// One style channel `MLR126` validates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StyleChannel {
    Opacity,
    StrokeWidth,
}

impl StyleChannel {
    const fn violates(self, value: f64) -> bool {
        match self {
            Self::Opacity => value < 0.0 || value > 1.0,
            Self::StrokeWidth => value < 0.0,
        }
    }
}

struct OpacityStrokeRange;

impl Rule for OpacityStrokeRange {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR126
    }

    #[allow(
        clippy::too_many_lines,
        reason = "constructor and method argument tables"
    )]
    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        let profiles = context.config().active_profile_names();
        let mut diagnostics = Vec::new();
        for call in &context.qualified_calls().calls {
            // (display name, channel, keyword name, argument fact)
            let mut checks: Vec<(String, StyleChannel, String, &CallArgument)> = Vec::new();
            let constructor_target =
                single_knowledge_symbol(profile, &call.candidates).filter(|(_, entry)| {
                    matches!(entry.kind, SymbolKind::Mobject | SymbolKind::Vmobject)
                });
            if let Some((constructor_id, _)) = constructor_target {
                let constructor = short_name(constructor_id);
                for (keyword, channel) in [
                    ("fill_opacity", StyleChannel::Opacity),
                    ("stroke_opacity", StyleChannel::Opacity),
                    ("opacity", StyleChannel::Opacity),
                    ("stroke_width", StyleChannel::StrokeWidth),
                ] {
                    if let Some(argument) = call.keyword(keyword) {
                        checks.push((
                            format!("{constructor}()"),
                            channel,
                            keyword.to_owned(),
                            argument,
                        ));
                    }
                }
            } else if matches!(call.receiver, ReceiverKind::KnownInstance(_)) {
                let Some((canonical, _)) = resolved_method_for_call(profile, call) else {
                    continue;
                };
                let method = short_name(&canonical).to_owned();
                // (keyword name, positional index, channel)
                let slots: &[(&str, usize, StyleChannel)] = match method.as_str() {
                    "set_opacity" => &[("opacity", 0, StyleChannel::Opacity)],
                    "set_fill" => &[("opacity", 1, StyleChannel::Opacity)],
                    "set_stroke" => &[
                        ("width", 1, StyleChannel::StrokeWidth),
                        ("opacity", 2, StyleChannel::Opacity),
                    ],
                    _ => continue,
                };
                for (keyword, position, channel) in slots {
                    let argument = call.keyword(keyword).or_else(|| {
                        if call.has_star_args {
                            None
                        } else {
                            call.positional(*position)
                                .filter(|argument| argument.keyword.is_none())
                        }
                    });
                    if let Some(argument) = argument {
                        checks.push((
                            format!(".{method}()"),
                            *channel,
                            (*keyword).to_owned(),
                            argument,
                        ));
                    }
                }
            } else {
                continue;
            }

            for (display, channel, keyword, argument) in checks {
                let Some(value) = numeric_literal(argument) else {
                    continue;
                };
                if !channel.violates(value) {
                    continue;
                }
                let file = context.sources().file(call.file);
                let written = file.slice(argument.range);
                let requirement = match channel {
                    StyleChannel::Opacity => "opacities are alpha values in [0.0, 1.0]",
                    StyleChannel::StrokeWidth => "stroke widths must be >= 0",
                };
                let mut evidence = BTreeMap::new();
                evidence.insert("call".to_owned(), json!(display));
                evidence.insert("keyword".to_owned(), json!(keyword));
                evidence.insert("value".to_owned(), json!(value));
                diagnostics.push(build_diagnostic(
                    &MLR126,
                    file,
                    argument.range,
                    Confidence::High,
                    format!("`{display}` {keyword}={written} is invalid: {requirement}"),
                    "Opacity is an alpha channel: values outside [0, 1] are not \
                     meaningful and render as clamped or corrupted colors. A \
                     negative stroke width is never a valid pen size.",
                    evidence,
                    profiles.clone(),
                    None,
                ));
            }
        }
        diagnostics
    }
}
