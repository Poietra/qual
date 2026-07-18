//! `MLP214`: serial construction of distinct TeX compile keys the local
//! fork could precompile in parallel (DESIGN §7.3).
//!
//! The rule is **local-fork-overlay only**: it stays inert unless the
//! selected knowledge profile declares the `tex_parallel_compile`
//! capability (`KnowledgeProfile::tex_parallel_compile`, `None` on
//! `upstream_0_20`), so it can never suggest an API the selected profile
//! does not have — the declared `entry_points` are validated to be curated
//! symbols of the resolved profile.
//!
//! Per the DESIGN §7.3 prose, the advice fires only when, on a **cold
//! cache**, at least [`MIN_DISTINCT_COMPILE_KEYS`] *distinct* compile keys
//! are serially constructed **before the scene's first play**:
//!
//! - Distinctness is decided by **literal-provable** compile keys only
//!   (every TeX argument a plain string literal, key-affecting keywords
//!   literal or absent). Constructing the same formula twice is ONE compile
//!   job; a dynamic (f-string / name / call) key is never countable and
//!   never contributes to the threshold.
//! - The cache model starts **cold at scene entry** (first render,
//!   `ResourceState`'s `CacheAssumption::Cold`, DESIGN §5.5); keys whose
//!   temperature cannot be proven (branch-dependent constructions) are
//!   *unknown*, not cold, and are not counted.
//! - The "before first use" ordering is read from the interpreter's event
//!   trace, so inlined helper constructions count and anything behind a
//!   summary fallback (which may hide a play) conservatively silences the
//!   scene.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::diagnostic::{Confidence, Diagnostic, RelatedLocation, RuleMetadata, Severity};
use crate::frontend::index::{LiteralFact, QualifiedCall};
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::events::Event;
use crate::semantic::values::{AllocationSite, KindSet, Presence};
use crate::source::FileId;

use super::support::{build_diagnostic, conclusive_target};

/// The DESIGN §7.3 emission gate: "cold cache で 4 件以上の distinct
/// compile key" — fewer distinct keys never fire.
const MIN_DISTINCT_COMPILE_KEYS: usize = 4;

/// TeX-compiling constructors with the fork's key-relevant defaults
/// (`tex_mobject.py`): `(canonical id, default arg separator, default
/// tex environment)`. The compile key of a construction is the
/// environment plus the separator-joined TeX strings, so two classes with
/// the same defaults and the same literal content share one compile job.
const TEX_COMPILE_CLASSES: [(&str, &str, &str); 3] = [
    ("manim.mobject.text.tex_mobject.MathTex", " ", "align*"),
    (
        "manim.mobject.text.tex_mobject.SingleStringMathTex",
        " ",
        "align*",
    ),
    ("manim.mobject.text.tex_mobject.Tex", "", "center"),
];

/// Keyword arguments that change the generated TeX file in ways this rule
/// does not model: their mere presence makes the key non-literal.
/// (`substrings_to_isolate` / `tex_to_color_map` re-split the joined
/// string; `tex_template` swaps the preamble.)
const UNMODELED_KEY_KEYWORDS: [&str; 3] =
    ["substrings_to_isolate", "tex_template", "tex_to_color_map"];

/// Metadata for [`SerialTexCompileKeys`].
pub const MLP214: RuleMetadata = RuleMetadata {
    id: "MLP214",
    summary: "Serial construction of distinct TeX compile keys the local fork could \
              precompile in parallel",
    default_enabled: true,
    default_severity: Severity::Info,
    minimum_confidence: Confidence::High,
    implementation_phase: 3,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "lifecycle", "local-fork-overlay"],
    supersedes: &[],
};

/// Distinct `MathTex` / `Tex` compile keys constructed serially before the
/// scene's first play, under a knowledge profile whose overlay declares
/// the submit-all/collect TeX API (DESIGN §7.3 `MLP214`).
pub struct SerialTexCompileKeys;

/// One provably cold compile job: its literal fingerprint and the first
/// on-every-path construction site.
struct CompileJob {
    /// `(environment, joined TeX content)` — equal fingerprints are one
    /// external compile job.
    fingerprint: (String, String),
    /// Index of the anchoring construction into the qualified-call facts.
    call_index: usize,
}

impl Rule for SerialTexCompileKeys {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLP214
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let Some(profile) = context.knowledge() else {
            return Vec::new();
        };
        // Inertness gate (DESIGN §7.3): without the curated fork
        // capability there is no submit-all API to cite.
        let Some(tex) = profile.tex_parallel_compile() else {
            return Vec::new();
        };
        let calls = context.qualified_calls();
        let call_by_site: BTreeMap<(FileId, u32, u32), usize> = calls
            .calls
            .iter()
            .enumerate()
            .map(|(call_index, call)| {
                (
                    (
                        call.file,
                        u32::from(call.call_range.start()),
                        u32::from(call.call_range.end()),
                    ),
                    call_index,
                )
            })
            .collect();

        let mut anchored: BTreeSet<(FileId, u32, u32)> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            // An unresolved MRO leaves the constructor state unknown; a
            // summary-applied helper may hide a play, destroying the
            // "before first use" ordering proof. Silence over a guess.
            if scene.constructor_state_unknown || !scene.summary_derived_plays.is_empty() {
                continue;
            }

            let mut jobs: Vec<CompileJob> = Vec::new();
            let mut first_use: Option<AllocationSite> = None;
            for traced in &scene.events {
                match &traced.event {
                    // The first play is the "first use" boundary: with the
                    // fork pipeline enabled every future must be collected
                    // before it. An unresolved call may itself reach
                    // `Scene.play`, so it bounds the provable prefix too.
                    Event::BeginPlay(_) | Event::UnknownMutation(_) => {
                        first_use = Some(traced.site);
                        break;
                    }
                    Event::Alloc(alloc) => {
                        // Cold-cache proof needs an on-every-path
                        // construction; a branch-dependent one leaves the
                        // key's temperature unknown, never cold.
                        if traced.certainty != Presence::Present {
                            continue;
                        }
                        let Some(job) =
                            compile_job(context, &call_by_site, &alloc.kind, traced.site)
                        else {
                            continue;
                        };
                        if !jobs
                            .iter()
                            .any(|existing| existing.fingerprint == job.fingerprint)
                        {
                            jobs.push(job);
                        }
                    }
                    _ => {}
                }
            }
            if jobs.len() < MIN_DISTINCT_COMPILE_KEYS {
                continue;
            }

            let anchor = &calls.calls[jobs[0].call_index];
            let anchor_key = (
                anchor.file,
                u32::from(anchor.call_range.start()),
                u32::from(anchor.call_range.end()),
            );
            // A helper inlined into several scenes anchors here once.
            if !anchored.insert(anchor_key) {
                continue;
            }
            diagnostics.push(scene_diagnostic(
                context,
                tex,
                &scene.qualified_name,
                &jobs,
                first_use,
            ));
        }
        diagnostics
    }
}

/// Builds the one per-scene diagnostic: anchored at the first counted
/// construction, the other distinct keys as related locations, every
/// count and curated fact in the evidence map.
fn scene_diagnostic(
    context: &RuleContext<'_>,
    tex: &crate::knowledge::TexParallelCompile,
    scene_name: &str,
    jobs: &[CompileJob],
    first_use: Option<AllocationSite>,
) -> Diagnostic {
    let calls = context.qualified_calls();
    let anchor = &calls.calls[jobs[0].call_index];
    let file = context.sources().file(anchor.file);

    let related_locations = jobs[1..]
        .iter()
        .map(|job| {
            let call = &calls.calls[job.call_index];
            let related_file = context.sources().file(call.file);
            RelatedLocation {
                path: related_file.relative_path().to_owned(),
                span: related_file.span_of_range(call.call_range),
                message: format!(
                    "distinct TeX compile key: {content}",
                    content = display_key(&job.fingerprint.1)
                ),
            }
        })
        .collect();

    let mut evidence = BTreeMap::new();
    evidence.insert(
        "distinct_compile_keys".to_owned(),
        Value::Number(jobs.len().into()),
    );
    evidence.insert(
        "compile_keys".to_owned(),
        Value::Array(
            jobs.iter()
                .map(|job| Value::String(display_key(&job.fingerprint.1)))
                .collect(),
        ),
    );
    evidence.insert(
        "entry_points".to_owned(),
        Value::Array(
            tex.entry_points
                .iter()
                .map(|entry| Value::String(entry.clone()))
                .collect(),
        ),
    );
    evidence.insert(
        "cache_assumption".to_owned(),
        Value::String(
            "cold (first render of the scene; an already-cached key resolves its \
             future immediately)"
                .to_owned(),
        ),
    );
    evidence.insert(
        "first_use".to_owned(),
        match first_use {
            Some(site) => Value::String(site_location(context, site)),
            None => Value::String("none (no play in this scene)".to_owned()),
        },
    );
    evidence.insert("scene".to_owned(), Value::String(scene_name.to_owned()));
    evidence.insert(
        "external_process".to_owned(),
        json!("TeX compiler + dvisvgm"),
    );

    let mut diagnostic = build_diagnostic(
        &MLP214,
        context,
        file,
        anchor.call_range,
        format!(
            "{count} distinct TeX compile keys are constructed serially before \
             this scene's first play: on a cold cache each construction blocks \
             on one external TeX compile, while the selected fork profile \
             offers a submit-all/collect API (`{entry}`).",
            count = jobs.len(),
            entry = tex.entry_points.join("`, `"),
        ),
        explanation(tex),
        evidence,
    );
    diagnostic.related_locations = related_locations;
    diagnostic
}

/// The advice text, citing only curated facts of the declared capability.
fn explanation(tex: &crate::knowledge::TexParallelCompile) -> String {
    let mut text = format!(
        "Each distinct cold TeX key launches an external compiler and `dvisvgm` \
         serially at construction time. The selected local-fork profile provides \
         `{entry}`: submit every formula first, then construct the mobjects — a \
         construction joins its in-flight job instead of compiling again.",
        entry = tex.entry_points.join("` / `"),
    );
    if tex.same_key_coalesced == Some(true) {
        text.push_str(" Same-key submissions are coalesced into one job.");
    }
    if tex.cache_hit_short_circuits == Some(true) {
        text.push_str(
            " An already-cached key returns an immediately resolved future, so the \
             pattern is safe under a warm cache too.",
        );
    }
    if tex.in_flight_blocks_cairo_fork == Some(true) {
        text.push_str(
            " Collect every future before the first play: in-flight TeX futures force \
             the fork's Cairo fork-per-play pipeline into serial fallback.",
        );
    }
    text.push_str(" No automatic fix is offered.");
    text
}

/// Renders one compile key for evidence, bounded for pathological inputs.
fn display_key(content: &str) -> String {
    const MAX: usize = 120;
    if content.chars().count() <= MAX {
        content.to_owned()
    } else {
        let prefix: String = content.chars().take(MAX).collect();
        format!("{prefix}…")
    }
}

/// `path:line:column` of an allocation-site byte offset.
fn site_location(context: &RuleContext<'_>, site: AllocationSite) -> String {
    let file = context.sources().file(site.file);
    let position = file.position_of_byte(site.start as usize);
    format!(
        "{path}:{line}:{column}",
        path = file.relative_path(),
        line = position.line,
        column = position.column,
    )
}

/// Resolves one traced allocation into a countable compile job: the
/// allocated kind and the call target must agree on a single curated TeX
/// class, and the compile key must be literal-provable.
fn compile_job(
    context: &RuleContext<'_>,
    call_by_site: &BTreeMap<(FileId, u32, u32), usize>,
    kind: &KindSet,
    site: AllocationSite,
) -> Option<CompileJob> {
    let KindSet::Known(kinds) = kind else {
        return None;
    };
    if kinds.len() != 1 {
        return None;
    }
    let canonical = kinds.iter().next()?;
    let (_, separator, environment) = TEX_COMPILE_CLASSES
        .iter()
        .find(|(id, _, _)| id == canonical)?;
    let call_index = *call_by_site.get(&(site.file, site.start, site.end))?;
    let call = &context.qualified_calls().calls[call_index];
    let (resolved, _) = conclusive_target(context.knowledge()?, context.project_index(), call)?;
    if resolved != *canonical {
        return None;
    }
    let fingerprint = literal_compile_key(call, separator, environment)?;
    Some(CompileJob {
        fingerprint,
        call_index,
    })
}

/// The literal-provable compile key of a TeX construction:
/// `(tex_environment, arg_separator.join(tex_strings))`. `None` whenever
/// any part of the key is not a plain string literal — a dynamic key is
/// never countable (DESIGN §15: silence over a fabricated count).
fn literal_compile_key(
    call: &QualifiedCall,
    default_separator: &str,
    default_environment: &str,
) -> Option<(String, String)> {
    // Splats can smuggle both extra TeX strings and key keywords.
    if call.has_star_args || call.has_star_star_kwargs || call.positional_count == 0 {
        return None;
    }
    let mut parts: Vec<&str> = Vec::with_capacity(call.positional_count);
    for position in 0..call.positional_count {
        let argument = call.positional(position)?;
        let Some(LiteralFact::Str { value, .. }) = &argument.literal else {
            return None;
        };
        parts.push(value);
    }
    if UNMODELED_KEY_KEYWORDS
        .iter()
        .any(|name| call.keyword_names.contains(*name))
    {
        return None;
    }
    let keyword_str = |name: &str| -> Option<Option<String>> {
        // Outer None: keyword present but not a string literal (give up);
        // inner Option: whether the keyword is present at all.
        match call
            .arguments
            .iter()
            .find(|argument| argument.keyword.as_deref() == Some(name))
        {
            None => Some(None),
            Some(argument) => match &argument.literal {
                Some(LiteralFact::Str { value, .. }) => Some(Some(value.clone())),
                _ => None,
            },
        }
    };
    let separator = keyword_str("arg_separator")?.unwrap_or_else(|| default_separator.to_owned());
    let environment =
        keyword_str("tex_environment")?.unwrap_or_else(|| default_environment.to_owned());
    Some((environment, parts.join(&separator)))
}
