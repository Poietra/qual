//! Shared fact-resolution helpers for the lifecycle rules.
//!
//! Every rule in this group fires only on *confirmed* facts (DESIGN §15
//! invariant 2): a call target counts as resolved only when every frontend
//! candidate maps to the same curated knowledge symbol, and receiver
//! classes with unresolved bases never support a diagnostic.

use std::collections::{BTreeMap, BTreeSet};

use rustpython_parser::text_size::TextRange;
use serde_json::Value;

use crate::diagnostic::{Diagnostic, RuleMetadata};
use crate::frontend::index::{ManimKind, ProjectIndex, QualifiedCall, ReceiverKind};
use crate::knowledge::{KnowledgeProfile, SymbolEntry, SymbolKind};
use crate::rules::base::RuleContext;
use crate::source::SourceFile;

/// Canonical id of `Scene.play`.
pub(super) const SCENE_PLAY: &str = "manim.scene.scene.Scene.play";
/// Canonical id of `Scene.wait`.
pub(super) const SCENE_WAIT: &str = "manim.scene.scene.Scene.wait";
/// Canonical id of `Scene.pause`.
pub(super) const SCENE_PAUSE: &str = "manim.scene.scene.Scene.pause";
/// Canonical id of `Scene.replace`.
pub(super) const SCENE_REPLACE: &str = "manim.scene.scene.Scene.replace";
/// Canonical id of `Scene.add_updater`.
pub(super) const SCENE_ADD_UPDATER: &str = "manim.scene.scene.Scene.add_updater";
/// Canonical id of `Mobject.add_updater`.
pub(super) const MOBJECT_ADD_UPDATER: &str = "manim.mobject.mobject.Mobject.add_updater";
/// Canonical id of `Mobject.add`.
pub(super) const MOBJECT_ADD: &str = "manim.mobject.mobject.Mobject.add";
/// Canonical id of the `Wait` animation class.
pub(super) const WAIT_ANIMATION: &str = "manim.animation.animation.Wait";
/// Canonical id of the `AnimationGroup` class.
pub(super) const ANIMATION_GROUP: &str = "manim.animation.composition.AnimationGroup";
/// Canonical id of the `Succession` class.
pub(super) const SUCCESSION: &str = "manim.animation.composition.Succession";
/// Canonical id of the `ApplyMethod` class.
pub(super) const APPLY_METHOD: &str = "manim.animation.transform.ApplyMethod";
/// Canonical id of the `VGroup` class.
pub(super) const VGROUP: &str = "manim.mobject.types.vectorized_mobject.VGroup";
/// Canonical id of the `Group` class.
pub(super) const GROUP: &str = "manim.mobject.mobject.Group";

/// Resolves one frontend candidate id to a curated knowledge entry.
///
/// A direct hit wins; otherwise a `Class.method` candidate is resolved by
/// walking the curated base-class chain (e.g.
/// `manim...Square.shift` → `manim.mobject.mobject.Mobject.shift`). Returns
/// `None` when the candidate reaches no curated symbol; callers must stay
/// silent then.
pub(super) fn resolve_symbol<'p>(
    profile: &'p KnowledgeProfile,
    candidate: &str,
) -> Option<(String, &'p SymbolEntry)> {
    if let Some(entry) = profile.symbol(candidate) {
        return Some((candidate.to_owned(), entry));
    }
    let (class_id, method) = candidate.rsplit_once('.')?;
    let mut queue: Vec<String> = vec![class_id.to_owned()];
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut cursor = 0;
    while cursor < queue.len() {
        let current = queue[cursor].clone();
        cursor += 1;
        if !visited.insert(current.clone()) {
            continue;
        }
        let qualified = format!("{current}.{method}");
        if let Some(entry) = profile.symbol(&qualified) {
            return Some((qualified, entry));
        }
        let Some(class_entry) = profile.symbol(&current) else {
            continue;
        };
        queue.extend(class_entry.bases.iter().cloned());
    }
    None
}

/// Whether the receiver classification of `call` is complete enough to
/// support a diagnostic: a project receiver class with unresolved bases may
/// inherit an override this index cannot see, so it never confirms a
/// curated target.
pub(super) fn receiver_conclusive(call: &QualifiedCall, index: &ProjectIndex) -> bool {
    let class = match &call.receiver {
        ReceiverKind::SelfScene => call.context.class_name.as_deref(),
        ReceiverKind::KnownInstance(kind) => Some(kind.as_str()),
        ReceiverKind::ModuleAlias | ReceiverKind::Direct => None,
        ReceiverKind::Unknown => return false,
    };
    match class.and_then(|id| index.classes.get(id)) {
        Some(record) => record.bases_fully_resolved,
        // External canonical class, or no receiver class at all.
        None => true,
    }
}

/// Resolves `call` to a single curated knowledge symbol.
///
/// Succeeds only when the candidate set is non-empty, the receiver is
/// conclusively classified, and *every* candidate resolves to the same
/// canonical id. Anything less is `None`: silence over a false positive.
pub(super) fn conclusive_target<'p>(
    profile: &'p KnowledgeProfile,
    index: &ProjectIndex,
    call: &QualifiedCall,
) -> Option<(String, &'p SymbolEntry)> {
    if call.candidates.is_empty() || !receiver_conclusive(call, index) {
        return None;
    }
    let mut resolved: Option<(String, &'p SymbolEntry)> = None;
    for candidate in &call.candidates {
        let (id, entry) = resolve_symbol(profile, candidate)?;
        match &resolved {
            None => resolved = Some((id, entry)),
            Some((existing, _)) if *existing == id => {}
            Some(_) => return None,
        }
    }
    resolved
}

/// Whether `call` is a bound method call (`self.play(...)`, `mob.add(...)`)
/// rather than an unbound class-attribute call.
pub(super) fn bound_receiver(call: &QualifiedCall) -> bool {
    matches!(
        call.receiver,
        ReceiverKind::SelfScene | ReceiverKind::KnownInstance(_)
    )
}

/// What a class id is known to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClassKind {
    /// A Scene class.
    Scene,
    /// A vectorized mobject (`VMobject` family).
    Vmobject,
    /// A plain mobject that is *not* vectorized (e.g. `ImageMobject`).
    MobjectOnly,
    /// An `Animation` class.
    Animation,
    /// A known class that is none of the Manim families.
    KnownOther,
}

/// A class classification plus whether it is complete.
///
/// `conclusive` is false when the class may reach bases this analysis does
/// not know (unresolved or uncurated); negative claims ("not a mobject",
/// "not vectorized") require `conclusive`, positive membership does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ClassFact {
    pub(super) kind: ClassKind,
    pub(super) conclusive: bool,
}

impl ClassFact {
    /// Certainly a mobject and certainly not anything else.
    pub(super) fn conclusive_mobject(self) -> bool {
        self.conclusive && matches!(self.kind, ClassKind::Vmobject | ClassKind::MobjectOnly)
    }
}

/// Classifies a class id: a curated knowledge class, or a project class
/// whose resolved base chain reaches curated bases. `None` when nothing is
/// known — callers must stay silent then.
pub(super) fn classify_class(
    profile: &KnowledgeProfile,
    index: &ProjectIndex,
    id: &str,
) -> Option<ClassFact> {
    if let Some(entry) = profile.symbol(id) {
        let kind = match entry.kind {
            SymbolKind::Scene => ClassKind::Scene,
            SymbolKind::Mobject => ClassKind::MobjectOnly,
            SymbolKind::Vmobject => ClassKind::Vmobject,
            SymbolKind::Animation => ClassKind::Animation,
            SymbolKind::Camera | SymbolKind::Function | SymbolKind::Constant => {
                ClassKind::KnownOther
            }
            SymbolKind::Method => return None,
        };
        return Some(ClassFact {
            kind,
            conclusive: true,
        });
    }
    let record = index.classes.get(id)?;
    let scene = record.kinds.contains(&ManimKind::Scene);
    let mobject = record.kinds.contains(&ManimKind::Mobject);
    let animation = record.kinds.contains(&ManimKind::Animation);
    let vectorized = record.reached_bases.iter().any(|base| {
        profile
            .symbol(base)
            .is_some_and(|entry| entry.kind == SymbolKind::Vmobject)
    });
    let all_bases_curated = record
        .reached_bases
        .iter()
        .all(|base| profile.symbol(base).is_some());
    let conclusive = record.bases_fully_resolved && all_bases_curated;
    let kind = match (scene, mobject, animation) {
        (true, false, false) => ClassKind::Scene,
        (false, true, false) => {
            if vectorized {
                ClassKind::Vmobject
            } else {
                ClassKind::MobjectOnly
            }
        }
        (false, false, true) => ClassKind::Animation,
        (false, false, false) => {
            if conclusive {
                ClassKind::KnownOther
            } else {
                return None;
            }
        }
        // Mixed roles: never a confirmed fact.
        _ => return None,
    };
    Some(ClassFact { kind, conclusive })
}

/// The class-id prefix of a canonical `Class.method` id.
pub(super) fn owner_class(canonical_method: &str) -> Option<&str> {
    canonical_method.rsplit_once('.').map(|(class, _)| class)
}

/// Whether the owner class of a canonical method id is a mobject family.
pub(super) fn owned_by_mobject(
    profile: &KnowledgeProfile,
    index: &ProjectIndex,
    canonical_method: &str,
) -> bool {
    owner_class(canonical_method)
        .and_then(|class| classify_class(profile, index, class))
        .is_some_and(|fact| matches!(fact.kind, ClassKind::Vmobject | ClassKind::MobjectOnly))
}

/// Builds a diagnostic with the rule's default severity and minimum
/// confidence; the caller attaches a fix afterwards when applicable.
pub(super) fn build_diagnostic(
    metadata: &'static RuleMetadata,
    context: &RuleContext<'_>,
    file: &SourceFile,
    range: TextRange,
    message: String,
    explanation: String,
    evidence: BTreeMap<String, Value>,
) -> Diagnostic {
    Diagnostic {
        rule_id: metadata.id.to_owned(),
        severity: metadata.default_severity,
        confidence: metadata.minimum_confidence,
        path: file.relative_path().to_owned(),
        primary_span: file.span_of_range(range),
        message,
        explanation: Some(explanation),
        related_locations: Vec::new(),
        evidence,
        estimated_cost: None,
        applicable_profiles: context.config().active_profile_names(),
        fix: None,
    }
}

/// The parser byte range of an interpreter allocation / event site.
pub(super) fn site_range(site: &crate::semantic::values::AllocationSite) -> TextRange {
    TextRange::new(site.start.into(), site.end.into())
}

/// Human-readable label of a write channel for messages and evidence.
pub(super) const fn channel_label(channel: crate::semantic::state::WriteChannel) -> &'static str {
    use crate::semantic::state::WriteChannel;
    match channel {
        WriteChannel::Points => "points",
        WriteChannel::Style => "style",
        WriteChannel::Opacity => "opacity",
        WriteChannel::Membership => "membership",
        WriteChannel::CameraState => "camera-state",
    }
}

/// Machine-readable candidate list of a call for the evidence map.
pub(super) fn candidates_value(call: &QualifiedCall) -> Value {
    Value::Array(
        call.candidates
            .iter()
            .map(|candidate| Value::String(candidate.clone()))
            .collect(),
    )
}

/// Whether callee source text is a simple dotted name (`sq.shift`), i.e.
/// safe to splice textually in a suggested fix.
pub(super) fn simple_dotted(text: &str) -> bool {
    text.contains('.')
        && !text.starts_with('.')
        && !text.ends_with('.')
        && text
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_' || character == '.')
}
