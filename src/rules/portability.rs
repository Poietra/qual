//! Determinism / portability / cache-stability rules (`MLD3xx`, DESIGN §7.4).
//!
//! Implemented: `MLD301` (FPS-dependent updater motion), `MLD302` (unseeded
//! global random state in frame callbacks), `MLD303` (foreign-platform
//! absolute asset paths), `MLD304` (renderer-divergent semantics without a
//! guard under a multi-renderer run), `MLD305` (case-only asset path
//! mismatches on case-sensitive target platforms), `MLD306` (fonts outside
//! the profile allowlist), and `MLD307` (wall-clock / filesystem / network
//! calls inside frame callbacks).
//!
//! Every rule fires only on confirmed facts: knowledge-resolved call
//! candidates, literal argument facts, proven hot contexts, definite
//! (all-paths) lifecycle events, and the frontend's statement / binding
//! facts (`frontend::statements`). Name resolution through imports is
//! conservative: a name rebound anywhere in the file is never trusted
//! (rebind poisoning lives in the frontend binding facts, DESIGN §5.3).
//! Unknown facts always mean silence (DESIGN §15 invariant 2).
//!
//! `build_diagnostic`, `single_knowledge_symbol`, and `short_name` are
//! shared with `rules::rendering` (the owning module exports them
//! `pub(crate)`).

mod fonts;
mod io_hooks;
mod paths;
mod randomness;
mod renderer_divergence;
mod updater_motion;

use crate::frontend::index::{ProjectIndex, QualifiedCall};
use crate::frontend::statements::FileBindingFacts;
use crate::rules::base::Rule;

/// Every implemented portability rule, in rule-ID order.
///
/// The registry composes this with the other rule-group modules; adding a
/// rule here is the only registration step a portability rule needs.
#[must_use]
pub fn rules() -> Vec<Box<dyn Rule>> {
    vec![
        Box::new(updater_motion::FpsDependentUpdaterMotion),
        Box::new(randomness::UnseededGlobalRandom),
        Box::new(paths::ForeignPlatformAssetPath),
        Box::new(renderer_divergence::UnguardedRendererDivergence),
        Box::new(paths::CaseOnlyAssetMismatch),
        Box::new(fonts::FontOutsideAllowlist),
        Box::new(io_hooks::FrameCallbackIo),
    ]
}

// ---------------------------------------------------------------------------
// Shared fact helpers (owned and exported by `rules::rendering`).
// ---------------------------------------------------------------------------

use crate::rules::rendering::{build_diagnostic, short_name, single_knowledge_symbol};

/// Canonical dotted targets of a call to a (potential) external-library
/// function.
///
/// The frontend resolves imports scope-correctly and records external
/// third-party callees as their dotted path (`import time` + `time.time()`
/// → candidate `time.time`), so candidates are preferred when present —
/// but only when no candidate can name a *project* symbol (a project
/// module `time` would produce the same string; never guess). Without
/// candidates the file's conservative binding facts resolve the callee's
/// dotted chain (`QualifiedCall::callee_dotted`).
fn external_call_targets(
    index: &ProjectIndex,
    call: &QualifiedCall,
    bindings: &FileBindingFacts,
) -> Vec<String> {
    if call.candidates.is_empty() {
        return call
            .callee_dotted
            .as_deref()
            .and_then(|parts| bindings.resolve_parts(parts))
            .into_iter()
            .collect();
    }
    let all_external = call.candidates.iter().all(|candidate| {
        let top = candidate.split('.').next().unwrap_or(candidate);
        !index.module_tree.contains(top)
    });
    if all_external {
        call.candidates.iter().cloned().collect()
    } else {
        Vec::new()
    }
}
