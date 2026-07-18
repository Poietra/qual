//! `MLR116`: `add_line_to` / `close_path` on a provably empty own path
//! (DESIGN §7.2).
//!
//! Both methods read the last / first entry of the receiver's `points`
//! array before appending (`vectorized_mobject.py`: `add_line_to` →
//! `add_cubic_bezier_curve_to(interpolate(self.get_last_point(), ...))`
//! with `get_last_point` indexing `points[-1]`; `close_path` →
//! `is_closed()` indexing `points[0]`), so calling either on an empty
//! path raises `IndexError` at render time.
//!
//! The interpreter proves emptiness through the curated empty-start
//! constructors (`VMobject()` / `VGroup()` / `Mobject()` / `Group()`) and
//! exact path-method arithmetic ([`SceneLifecycle::path_state_at`]); the
//! rule fires only on [`PathStateFact::empty`] == [`Truth::Yes`]. A
//! branch-dependent `start_new_path` joins the count to an interval and
//! the verdict to `Maybe`: silence (DESIGN §15 invariant 2).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;

use crate::diagnostic::{Confidence, Diagnostic, RuleMetadata, Severity};
use crate::rules::base::{Rule, RuleContext};
use crate::semantic::events::{Event, MutationKind};
use crate::semantic::values::Truth;
use crate::source::FileId;

use super::{build_diagnostic, call_at, site_range};

const MLR116: RuleMetadata = RuleMetadata {
    id: "MLR116",
    summary: "add_line_to/close_path on a provably empty path raises at render time",
    default_enabled: true,
    default_severity: Severity::Error,
    minimum_confidence: Confidence::High,
    implementation_phase: 2,
    required_profiles: &[],
    required_capabilities: &["qualified-calls", "lifecycle"],
    supersedes: &[],
};

pub(super) struct EmptyPathEdit;

impl Rule for EmptyPathEdit {
    fn metadata(&self) -> &'static RuleMetadata {
        &MLR116
    }

    fn run(&self, context: &RuleContext<'_>) -> Vec<Diagnostic> {
        let calls = context.qualified_calls();
        let profiles = context.config().active_profile_names();
        let mut seen: BTreeSet<(FileId, u32, u32)> = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for scene in &context.lifecycle_facts().scenes {
            for traced in &scene.events {
                let Event::Mutate(mutate) = &traced.event else {
                    continue;
                };
                if mutate.kind != MutationKind::PathTopology {
                    continue;
                }
                // The interpreter emitted this only for a confirmed
                // VMobject path method; the method name selects the two
                // that read existing points before appending.
                let Some(call) = call_at(calls, traced.site) else {
                    continue;
                };
                let file = context.sources().file(traced.site.file);
                let callee = file.slice(call.callee_range);
                let method = callee.rsplit('.').next().unwrap_or(callee);
                if method != "add_line_to" && method != "close_path" {
                    continue;
                }
                let Some(path_state) =
                    scene.path_state_at(&mutate.target, traced.site.file, traced.site.start)
                else {
                    continue;
                };
                // Only a provably empty own path fires; Maybe is silence.
                if path_state.empty != Truth::Yes {
                    continue;
                }
                if !seen.insert((traced.site.file, traced.site.start, traced.site.end)) {
                    continue;
                }
                let mut evidence = BTreeMap::new();
                evidence.insert("method".to_owned(), json!(method));
                evidence.insert(
                    "point_count".to_owned(),
                    json!({
                        "lower": path_state.point_count.lower_bound(),
                        "upper": path_state.point_count.upper_bound(),
                    }),
                );
                let read = if method == "add_line_to" {
                    "reads the path's last point as the new line's start"
                } else {
                    "reads the path's first point to close back to"
                };
                diagnostics.push(build_diagnostic(
                    &MLR116,
                    file,
                    site_range(traced.site),
                    Confidence::High,
                    format!(
                        "`.{method}()` on a provably empty path: the call {read} and \
                         raises IndexError at render time; start the path with \
                         `start_new_path(point)` (or `set_points_as_corners`) first"
                    ),
                    "A fresh VMobject/VGroup starts with an empty points array. \
                     add_line_to extends the current curve from the last anchor and \
                     close_path connects back to the first anchor, so both index \
                     into `points` before appending anything. With no prior \
                     start_new_path / set_points call on any path to this \
                     statement, the index lookup fails the moment the scene is \
                     rendered.",
                    evidence,
                    profiles.clone(),
                    None,
                ));
            }
        }
        diagnostics
    }
}
