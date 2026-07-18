//! Transform begin-cost rules: `MLP207` and `MLP208` (DESIGN §7.3).
//!
//! Both walk the played animations of every analyzed scene and consider
//! only provable two-mobject Transform-family constructions (the curated
//! classes that copy the source, align point/curve topology with the
//! target, and interpolate every family member at `Animation.begin`).
//!
//! `MLP207` needs the DESIGN §7.3 numeric gate
//! ([`transform_begin_gate_met`]): a confirmed source family of at least
//! 32 or a confirmed curve insertion of at least 256, both measured at
//! the play site. `MLP208` is the *large* Text / TeX specialization
//! (DESIGN §7.3 catalog row): the kind test selects it, and the size
//! gate is met either by the structural numbers (when they are real) or
//! by a lower bound counted from the literal constructor content —
//! glyph families are content-dependent, so a small title transform
//! stays silent and no number is ever fabricated. `MLP208` supersedes
//! `MLP207` on the same animation.

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::text_size::TextRange;
use serde_json::Value;

use crate::cost::estimator::num_bounds_json;
use crate::cost::thresholds::{
    TRANSFORM_CURVE_INSERTION_GATE, TRANSFORM_FAMILY_GATE, transform_begin_gate_met,
};
use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::frontend::index::{LiteralFact, QualifiedCall};
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::interpreter::{PlayFact, PlayedAnimation, SceneLifecycle};
use crate::semantic::values::{KindSet, Num, ObjectId, Presence, Truth};

use super::support::build_diagnostic;

/// Curated two-mobject Transform-family constructors: `Transform(source,
/// target)` and its topology-aligning relatives (`transform.py` /
/// `transform_matching_parts.py`). `ApplyMethod` / `MoveToTarget` /
/// `Restore` take no second mobject and never reach the replacement-target
/// fact.
const TRANSFORM_CLASSES: [&str; 5] = [
    "manim.animation.transform.ReplacementTransform",
    "manim.animation.transform.Transform",
    "manim.animation.transform_matching_parts.TransformMatchingAbstractBase",
    "manim.animation.transform_matching_parts.TransformMatchingShapes",
    "manim.animation.transform_matching_parts.TransformMatchingTex",
];

/// Canonical ids of the content-dependent Text / TeX classes whose
/// families are typically large and whose Transforms `MLP208`
/// specializes on.
const TEXT_TEX_CLASSES: [&str; 5] = [
    "manim.mobject.text.tex_mobject.MathTex",
    "manim.mobject.text.tex_mobject.SingleStringMathTex",
    "manim.mobject.text.tex_mobject.Tex",
    "manim.mobject.text.text_mobject.MarkupText",
    "manim.mobject.text.text_mobject.Text",
];

/// The TeX-compiled subset of [`TEXT_TEX_CLASSES`]: their literal content
/// is TeX source, so syntax characters and control sequences must not be
/// counted as rendered glyphs.
const TEX_CLASSES: [&str; 3] = [
    "manim.mobject.text.tex_mobject.MathTex",
    "manim.mobject.text.tex_mobject.SingleStringMathTex",
    "manim.mobject.text.tex_mobject.Tex",
];

/// The Pango-markup member of [`TEXT_TEX_CLASSES`]: tags and entities in
/// its literal content do not render one glyph per character.
const MARKUP_TEXT: &str = "manim.mobject.text.text_mobject.MarkupText";

/// `MLP208` content gate (DESIGN §7.3 `MLP207`/`MLP208` gate prose:
/// "start from `N_family >= 32`"): the literal constructor content must
/// prove at least this many rendered content characters before the
/// specialization fires — a short title transform is idiomatic and
/// cheap, not the catalog's "large Text / `MathTex` family".
pub const MLP208_CONTENT_GATE: usize = 32;

/// One provable Transform-family play argument: the single live source,
/// the tracked replacement target, and the play it belongs to.
struct TransformFact<'l> {
    scene: &'l SceneLifecycle,
    play: &'l PlayFact,
    animation: &'l PlayedAnimation,
    source: &'l ObjectId,
    target: &'l ObjectId,
}

/// Walks every played animation that is provably a two-mobject
/// Transform-family construction on a definite path, in deterministic
/// scene / play / argument order.
fn provable_transforms<'l>(context: &'l RuleContext<'_>) -> Vec<TransformFact<'l>> {
    let mut facts = Vec::new();
    for scene in &context.lifecycle_facts().scenes {
        for play in &scene.plays {
            // Maybe-path plays never support a definite begin-cost claim.
            if play.certainty != Presence::Present {
                continue;
            }
            for animation in &play.animations {
                if animation.convertible != Truth::Yes {
                    continue;
                }
                let Some(state) = &animation.state else {
                    continue;
                };
                let KindSet::Known(kinds) = &state.kind else {
                    continue;
                };
                if kinds.is_empty()
                    || !kinds
                        .iter()
                        .all(|kind| TRANSFORM_CLASSES.contains(&kind.as_str()))
                {
                    continue;
                }
                let Some(target) = &animation.replacement_target else {
                    continue;
                };
                if state.targets.len() != 1 {
                    continue;
                }
                let source = state.targets.first().expect("length checked");
                facts.push(TransformFact {
                    scene,
                    play,
                    animation,
                    source,
                    target,
                });
            }
        }
    }
    facts
}

/// The class candidates of `object` in the scene's final heap, when the
/// kind is definitely known. `None` for unknown kinds — never guessed.
fn known_kinds<'l>(scene: &'l SceneLifecycle, object: &ObjectId) -> Option<&'l BTreeSet<String>> {
    let resolved = scene.final_heap.resolve(object);
    let state = scene.final_heap.object(&resolved)?;
    match &state.kind {
        KindSet::Known(candidates) if !candidates.is_empty() => Some(candidates),
        _ => None,
    }
}

/// Whether every kind candidate of `object` is a curated Text / TeX class.
fn is_text_kind(scene: &SceneLifecycle, object: &ObjectId) -> bool {
    known_kinds(scene, object).is_some_and(|kinds| {
        kinds
            .iter()
            .all(|kind| TEXT_TEX_CLASSES.contains(&kind.as_str()))
    })
}

/// Whether `MLP208` claims this transform (source or target definitely a
/// Text / TeX family). `MLP207` skips claimed animations so one defect
/// yields exactly one diagnostic.
fn claimed_by_text_specialization(fact: &TransformFact<'_>) -> bool {
    is_text_kind(fact.scene, fact.source) || is_text_kind(fact.scene, fact.target)
}

/// Source-family size and estimated curve insertion of a transform, both
/// measured at the play site ([`Num::Unknown`] when not provable).
fn transform_sizes(context: &RuleContext<'_>, fact: &TransformFact<'_>) -> (Num, Num) {
    let cost = context.cost_facts();
    let delta =
        cost.curve_delta_for_transform(&fact.scene.qualified_name, fact.play, fact.animation);
    let family = cost
        .sizes
        .scene(&fact.scene.qualified_name)
        .map_or(Num::Unknown, |sizes| {
            sizes.sizes_at(fact.source, Some(fact.play.site)).family
        });
    (family, delta)
}

/// The qualified call whose whole expression is `object`'s allocation
/// site — the constructor call that produced it, when the frontend saw
/// one there.
fn constructor_call<'l>(
    context: &'l RuleContext<'_>,
    object: &ObjectId,
) -> Option<&'l QualifiedCall> {
    context
        .qualified_calls()
        .calls_in_file(object.site.file)
        .find(|call| {
            u32::from(call.call_range.start()) == object.site.start
                && u32::from(call.call_range.end()) == object.site.end
        })
}

/// Content characters of a literal TeX source string that provably
/// render: everything except whitespace, TeX syntax characters
/// (`{ } $ & # ^ _ ~`), comments (`%` to end of line), and control
/// sequences (`\word` and `\<symbol>` count zero — a conservative lower
/// bound, since e.g. `\alpha` renders one glyph).
fn tex_content_chars(value: &str) -> usize {
    let mut count = 0;
    let mut chars = value.chars().peekable();
    while let Some(current) = chars.next() {
        match current {
            '\\' => {
                if chars.peek().is_some_and(char::is_ascii_alphabetic) {
                    while chars.peek().is_some_and(char::is_ascii_alphabetic) {
                        chars.next();
                    }
                } else {
                    chars.next();
                }
            }
            '%' => {
                for skipped in chars.by_ref() {
                    if skipped == '\n' {
                        break;
                    }
                }
            }
            '{' | '}' | '$' | '&' | '#' | '^' | '_' | '~' => {}
            current if current.is_whitespace() => {}
            _ => count += 1,
        }
    }
    count
}

/// Content characters of a literal Pango text string: non-whitespace
/// characters, with markup tags (`<...>`) skipped and each entity
/// (`&...;`) counted as one character when `markup` is set.
fn text_content_chars(value: &str, markup: bool) -> usize {
    let mut count = 0;
    let mut chars = value.chars().peekable();
    while let Some(current) = chars.next() {
        if markup && current == '<' {
            for skipped in chars.by_ref() {
                if skipped == '>' {
                    break;
                }
            }
            continue;
        }
        if markup && current == '&' {
            while let Some(&next) = chars.peek() {
                if next == ';' || next.is_whitespace() {
                    chars.next();
                    break;
                }
                chars.next();
            }
            count += 1;
            continue;
        }
        if !current.is_whitespace() {
            count += 1;
        }
    }
    count
}

/// A proven lower bound on `object`'s rendered content characters,
/// counted from the literal positional string arguments of its
/// constructor call. `None` when the object is not a definite Text / TeX
/// kind or no literal content is visible — never a guess. Non-literal
/// arguments contribute nothing, so the bound only under-counts.
fn content_chars_lower_bound(
    context: &RuleContext<'_>,
    scene: &SceneLifecycle,
    object: &ObjectId,
) -> Option<usize> {
    let kinds = known_kinds(scene, object)?;
    if !kinds
        .iter()
        .all(|kind| TEXT_TEX_CLASSES.contains(&kind.as_str()))
    {
        return None;
    }
    let call = constructor_call(context, object)?;
    let mut minimum: Option<usize> = None;
    // Every kind candidate must prove the bound: the weakest candidate's
    // count is the sound lower bound.
    for kind in kinds {
        let counter: fn(&str) -> usize = if TEX_CLASSES.contains(&kind.as_str()) {
            tex_content_chars
        } else if kind == MARKUP_TEXT {
            |value: &str| text_content_chars(value, true)
        } else {
            |value: &str| text_content_chars(value, false)
        };
        let mut total = 0;
        for index in 0..call.positional_count {
            let Some(argument) = call.positional(index) else {
                continue;
            };
            if let Some(LiteralFact::Str { value, .. }) = &argument.literal {
                total += counter(value);
            }
        }
        minimum = Some(minimum.map_or(total, |current| current.min(total)));
    }
    minimum.filter(|&total| total > 0)
}

/// The diagnostic anchor: the animation argument expression.
fn animation_range(fact: &TransformFact<'_>) -> TextRange {
    TextRange::new(
        fact.animation.site.start.into(),
        fact.animation.site.end.into(),
    )
}

/// Shared evidence: kind labels plus family / curve-insertion bounds when
/// real (unknown quantities are omitted, never fabricated).
fn transform_evidence(
    fact: &TransformFact<'_>,
    family: &Num,
    delta: &Num,
    with_gates: bool,
) -> BTreeMap<String, Value> {
    let mut evidence = BTreeMap::new();
    evidence.insert(
        "scene".to_owned(),
        Value::String(fact.scene.qualified_name.clone()),
    );
    if let Some(kinds) = known_kinds(fact.scene, fact.source) {
        evidence.insert(
            "source_kind".to_owned(),
            Value::Array(kinds.iter().cloned().map(Value::String).collect()),
        );
    }
    if let Some(kinds) = known_kinds(fact.scene, fact.target) {
        evidence.insert(
            "target_kind".to_owned(),
            Value::Array(kinds.iter().cloned().map(Value::String).collect()),
        );
    }
    for (key, value) in [("family_size", family), ("curve_insertion", delta)] {
        let bounds = num_bounds_json(value);
        if !bounds.is_null() {
            evidence.insert(key.to_owned(), bounds);
        }
    }
    if with_gates {
        evidence.insert(
            "gates".to_owned(),
            Value::Array(vec![
                TRANSFORM_FAMILY_GATE.evidence(family),
                TRANSFORM_CURVE_INSERTION_GATE.evidence(delta),
            ]),
        );
    }
    evidence
}

// ---------------------------------------------------------------------------
// MLP207: confirmed-large topology / family mismatch at Transform begin.
// ---------------------------------------------------------------------------

/// Metadata for [`MismatchedTransformBegin`].
pub const MLP207: RuleMetadata = RuleMetadata {
    id: "MLP207",
    summary: "Transform whose confirmed family size or curve insertion is large at begin",
    default_enabled: true,
    default_severity: Severity::Info,
    minimum_confidence: Confidence::Medium,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["lifecycle", "cost-facts"],
    supersedes: &[],
};

/// A played Transform whose begin-time alignment cost is confirmed large:
/// source family of at least 32 members, or an estimated curve insertion
/// of at least 256, measured at the play site (DESIGN §7.3 `MLP207`).
pub struct MismatchedTransformBegin;

impl Rule for MismatchedTransformBegin {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP207
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        if context.knowledge().is_none() {
            return Vec::new();
        }
        let mut seen = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for fact in provable_transforms(context) {
            // The Text / TeX specialization owns these (MLP208 supersedes
            // MLP207): one defect, one diagnostic.
            if claimed_by_text_specialization(&fact) || !seen.insert(fact.animation.site) {
                continue;
            }
            let (family, delta) = transform_sizes(context, &fact);
            if !transform_begin_gate_met(&family, &delta) {
                continue;
            }
            let file = context.sources().file(fact.animation.site.file);
            let evidence = transform_evidence(&fact, &family, &delta, true);
            let quantified = if let Some(lower) = family.lower_bound() {
                format!("a source family of at least {lower:.0} members")
            } else if let Some(lower) = delta.lower_bound() {
                format!("an estimated insertion of at least {lower:.0} curves")
            } else {
                // Unreachable: the gate only passes on a proven bound.
                "a confirmed large alignment".to_owned()
            };
            diagnostics.push(build_diagnostic(
                &MLP207,
                context,
                file,
                animation_range(&fact),
                format!(
                    "This Transform pays a large one-time begin cost: {quantified} \
                     must be copied, aligned, and interpolated when the play \
                     starts."
                ),
                "At `Animation.begin` a Transform copies the source, aligns \
                 point and curve topology with the target (inserting curves \
                 into the smaller side), and interpolates every family member \
                 pair. With a large family or a large topology mismatch that \
                 setup cost is paid before the first frame renders. Consider \
                 transforming the specific submobjects that change, or using \
                 `FadeTransform` when per-curve alignment is not needed."
                    .to_owned(),
                evidence,
            ));
        }
        diagnostics
    }
}

// ---------------------------------------------------------------------------
// MLP208: Transform of a Text / MathTex family.
// ---------------------------------------------------------------------------

/// Metadata for [`TextFamilyTransform`].
pub const MLP208: RuleMetadata = RuleMetadata {
    id: "MLP208",
    summary: "Transform of a large Text/MathTex family (copy + align + per-glyph interpolation)",
    default_enabled: true,
    default_severity: Severity::Info,
    minimum_confidence: Confidence::High,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["lifecycle", "cost-facts"],
    // DESIGN §7.3: when both the numeric gate and the Text / TeX kind
    // test hold, exactly one specialized MLP208 is reported (MLP207 also
    // pre-filters the claimed animations).
    supersedes: &["MLP207"],
};

/// A played Transform whose source or target is definitely a Text / TeX
/// family (DESIGN §7.3 `MLP208`): glyph families make begin-time copy,
/// alignment, and per-submobject interpolation expensive. Text-family
/// cardinalities are content-dependent, so the evidence carries kind
/// facts and only such numeric bounds as are actually proven.
pub struct TextFamilyTransform;

impl Rule for TextFamilyTransform {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP208
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        if context.knowledge().is_none() {
            return Vec::new();
        }
        let mut seen = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for fact in provable_transforms(context) {
            if !claimed_by_text_specialization(&fact) || !seen.insert(fact.animation.site) {
                continue;
            }
            let (family, delta) = transform_sizes(context, &fact);
            // A content-dependent source decomposes into per-glyph
            // submobjects the structural graph cannot see, so its
            // structural family count is not meaningful evidence — omit
            // it rather than misstate the glyph family (DESIGN §15).
            let family = if is_text_kind(fact.scene, fact.source) {
                Num::Unknown
            } else {
                family
            };
            // DESIGN §7.3 catalog row: the specialization is for a
            // *large* Text / MathTex family. The gate is met by the
            // structural numbers when they are real, or by a proven
            // lower bound on the literal constructor content of either
            // side; a short title transform stays silent.
            let content_chars = [fact.source, fact.target]
                .into_iter()
                .filter_map(|object| content_chars_lower_bound(context, fact.scene, object))
                .max();
            let content_gate_met = content_chars.is_some_and(|chars| chars >= MLP208_CONTENT_GATE);
            if !content_gate_met && !transform_begin_gate_met(&family, &delta) {
                continue;
            }
            let file = context.sources().file(fact.animation.site.file);
            let mut evidence = transform_evidence(&fact, &family, &delta, false);
            if let Some(chars) = content_chars {
                evidence.insert(
                    "content_gate".to_owned(),
                    serde_json::json!({
                        "quantity": "literal content characters (lower bound)",
                        "minimum": MLP208_CONTENT_GATE,
                        "proven": chars,
                        "confirmed": chars >= MLP208_CONTENT_GATE,
                    }),
                );
            }
            diagnostics.push(build_diagnostic(
                &MLP208,
                context,
                file,
                animation_range(&fact),
                "This Transform operates on a Text/MathTex family: begin \
                 copies the whole glyph family, aligns each submobject's \
                 point data, and interpolates every glyph pair per frame."
                    .to_owned(),
                "Text and TeX mobjects decompose into one submobject per \
                 glyph, so transforming them multiplies the copy / align / \
                 interpolate cost by the glyph count. When the old and new \
                 content share most glyphs, `TransformMatchingTex` / \
                 `TransformMatchingShapes` moves matching glyphs instead of \
                 re-aligning everything; when only a numeric value changes, \
                 drive a `DecimalNumber` with `set_value`. The glyph count \
                 depends on the rendered content and is not statically \
                 provable, so this diagnostic quantifies only what is proven."
                    .to_owned(),
                evidence,
            ));
        }
        diagnostics
    }
}
