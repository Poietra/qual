//! `MLD302`: unseeded global random state read inside a frame callback
//! (DESIGN §7.4).
//!
//! A `random.random()` / `np.random.uniform()`-style call reads the
//! process-global random state. Inside a proven per-frame context every
//! render then produces different frames — the output is not reproducible
//! and frame caching / diffing breaks.
//!
//! The DESIGN §7.4 prose is binding:
//!
//! - literal-seeded local generators (`random.Random(seed)`, `numpy`
//!   `default_rng(seed)` / `Generator`) are **not** flagged — instance
//!   method calls never match the module-level patterns here;
//! - a module-level `random.seed(...)` (or `np.random.seed(...)`) call
//!   anywhere in the file downgrades the corresponding family to silence
//!   (conservative: the state may then be deterministic).
//!
//! Name resolution goes through the file's imports only; a rebound name is
//! never trusted (silence).

use std::collections::BTreeMap;

use serde_json::json;

use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::frontend::index::QualifiedCallFacts;
use crate::frontend::statements::FileBindingFacts;
use crate::rules::base::{Rule, RuleContext};
use crate::source::FileId;

use super::build_diagnostic;

/// Module-level functions of stdlib `random` that read the global state.
const RANDOM_STATE_READERS: [&str; 20] = [
    "betavariate",
    "choice",
    "choices",
    "expovariate",
    "gammavariate",
    "gauss",
    "getrandbits",
    "lognormvariate",
    "normalvariate",
    "paretovariate",
    "randbytes",
    "randint",
    "random",
    "randrange",
    "sample",
    "shuffle",
    "triangular",
    "uniform",
    "vonmisesvariate",
    "weibullvariate",
];

/// `numpy.random` attributes that are *not* global-state reads (seeding,
/// generator construction, state plumbing).
const NUMPY_NON_READERS: [&str; 10] = [
    "BitGenerator",
    "Generator",
    "MT19937",
    "PCG64",
    "Philox",
    "RandomState",
    "SFC64",
    "SeedSequence",
    "default_rng",
    "seed",
];

pub(super) const MLD302: RuleMetadata = RuleMetadata {
    id: "MLD302",
    summary: "Unseeded global random state read inside a frame callback",
    default_enabled: true,
    default_severity: Severity::Warning,
    minimum_confidence: Confidence::Medium,
    implementation_phase: 4,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "cost-facts"],
    supersedes: &[],
};

pub(super) struct UnseededGlobalRandom;

/// Per-file seed facts.
struct SeedFacts {
    /// A `random.seed(...)` call exists somewhere in the file.
    stdlib_seeded: bool,
    /// A `numpy.random.seed(...)` call exists somewhere in the file.
    numpy_seeded: bool,
}

impl SeedFacts {
    fn build(facts: &QualifiedCallFacts, bindings: &FileBindingFacts, file: FileId) -> Self {
        let mut stdlib_seeded = false;
        let mut numpy_seeded = false;
        for call in facts.calls_in_file(file) {
            if call.arguments.is_empty() {
                continue;
            }
            let resolved = call
                .callee_dotted
                .as_deref()
                .and_then(|parts| bindings.resolve_parts(parts));
            match resolved.as_deref() {
                Some("random.seed") => stdlib_seeded = true,
                Some("numpy.random.seed") => numpy_seeded = true,
                _ => {}
            }
        }
        Self {
            stdlib_seeded,
            numpy_seeded,
        }
    }
}

impl Rule for UnseededGlobalRandom {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLD302
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let cost = context.cost_facts();
        let call_facts = context.qualified_calls();
        let profiles = context.config().active_profile_names();
        let mut per_file: BTreeMap<FileId, SeedFacts> = BTreeMap::new();
        let mut diagnostics = Vec::new();
        for (call_index, call) in call_facts.calls.iter().enumerate() {
            let Some(hot) = cost.is_call_in_hot_context(call_index) else {
                continue;
            };
            let file = context.sources().file(call.file);
            let Some(bindings) = context.binding_facts().file(call.file) else {
                continue;
            };
            let seeds = per_file
                .entry(call.file)
                .or_insert_with(|| SeedFacts::build(call_facts, bindings, call.file));
            let targets = super::external_call_targets(context.project_index(), call, bindings);
            if targets.is_empty() {
                continue;
            }
            // Every resolution of the callee must be the same global
            // random-state read; anything else is silence.
            let classified: Vec<String> = targets
                .iter()
                .filter_map(|target| classify_global_random(target, seeds))
                .collect();
            if classified.len() != targets.len() {
                continue;
            }
            let Some(shown) = classified.first().cloned() else {
                continue;
            };
            if classified.iter().any(|name| *name != shown) {
                continue;
            }
            let mut evidence = BTreeMap::new();
            evidence.insert("function".to_owned(), json!(shown));
            evidence.insert("entry".to_owned(), json!(hot.entry.label()));
            evidence.insert("state_path".to_owned(), json!(hot.state_path()));
            diagnostics.push(build_diagnostic(
                &MLD302,
                file,
                call.call_range,
                Confidence::Medium,
                format!(
                    "`{shown}` reads the unseeded global random state inside a per-frame \
                     callback; every render likely produces different frames"
                ),
                "This call runs on the frame grid and draws from the process-global \
                 random state, so the rendered output changes between runs and frame \
                 caching cannot be reproduced. Seed the global state once at module \
                 level, or use a locally seeded generator (random.Random(seed), \
                 numpy.random.default_rng(seed)) — seeded local generators are not \
                 flagged by this rule.",
                evidence,
                profiles.clone(),
                None,
            ));
        }
        diagnostics
    }
}

/// Classifies a canonical dotted call target as a global random-state
/// read, honoring the per-file seed downgrades. Returns the display name.
fn classify_global_random(target: &str, seeds: &SeedFacts) -> Option<String> {
    if let Some(function) = target.strip_prefix("random.") {
        if !function.contains('.')
            && RANDOM_STATE_READERS.contains(&function)
            && !seeds.stdlib_seeded
        {
            return Some(format!("random.{function}"));
        }
        return None;
    }
    if let Some(function) = target.strip_prefix("numpy.random.") {
        if !function.contains('.') && !NUMPY_NON_READERS.contains(&function) && !seeds.numpy_seeded
        {
            return Some(format!("numpy.random.{function}"));
        }
    }
    None
}
